//! Domain HTTPS readiness product primitive.

mod readiness;

use std::future::Future;
use std::time::SystemTime;

use thiserror::Error;

use crate::acme::{
    CertificateDeadline, CertificatePort, CertificateStatus, CertificateUnusableReason,
    CertificateUsability, CertificateWaitReason, EnsureCertificateOutcome, Hostname, HttpsBinding,
    certificate_unusable_reason,
};
use crate::error::{CertificateFailure, ServingFailure};
use crate::operation::{ClaimGuard, MutationContext, TypedResourceId};
#[cfg(feature = "test-support")]
use crate::operation::{ClaimHash, FenceEpoch, PrincipalId};
use crate::serving::{
    RouteId, ServingCheckpoint, ServingCommitRequest, ServingGeneration, ServingTarget,
};

pub use readiness::{DomainReadinessPort, DomainReadinessService};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainName(String);

impl DomainName {
    pub fn parse(value: impl Into<String>) -> Result<Self, DomainFailure> {
        let value = value.into();
        let value = value.trim();
        if !is_valid_domain_name(value) {
            return Err(DomainFailure::InvalidDomain);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn https_binding(&self) -> Result<HttpsBinding, DomainFailure> {
        let hostname = Hostname::parse(self.0.clone()).map_err(|_| DomainFailure::InvalidDomain)?;
        Ok(HttpsBinding::new(hostname))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainAdd {
    pub domain: DomainName,
    pub certificate_policy: CertificatePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificatePolicy {
    pub minimum_valid_until: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainStatus {
    Ready(DomainReadyRecord),
    Pending(DomainPendingReason),
    Failed(DomainStatusFailure),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainPendingReason {
    CertificateIssuance,
    CertificateIssuanceInterrupted,
    ServingActivation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainReadinessOutcome {
    Ready(DomainReady),
    Pending(DomainPendingReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainStatusFailure {
    InvalidDomain,
    ClaimRejected,
    ClaimResourceMismatch,
    StaleClaim,
    CertificateUnusable(CertificateUnusableReason),
    CertificateFailed(CertificateFailure),
    ServingActivationFailed,
    ServingFailed(ServingFailure),
}

impl DomainStatusFailure {
    #[must_use]
    pub fn from_operation_failure(failure: &DomainFailure) -> Option<Self> {
        match failure {
            DomainFailure::InvalidDomain => Some(Self::InvalidDomain),
            DomainFailure::ClaimRejected => Some(Self::ClaimRejected),
            DomainFailure::ClaimResourceMismatch => Some(Self::ClaimResourceMismatch),
            DomainFailure::StaleClaim => Some(Self::StaleClaim),
            DomainFailure::CertificateUnusable(reason) => {
                Some(Self::CertificateUnusable(reason.clone()))
            }
            DomainFailure::CertificateFailed(failure) => {
                Some(Self::CertificateFailed(failure.clone()))
            }
            DomainFailure::ServingActivationFailed => Some(Self::ServingActivationFailed),
            DomainFailure::ServingFailed(failure) => Some(Self::ServingFailed(failure.clone())),
            DomainFailure::StatusUnavailable
            | DomainFailure::StatusRowsPayloadInvalid
            | DomainFailure::StatusRowsTimeout
            | DomainFailure::StatusRowsStreamInterrupted
            | DomainFailure::StatusRowsMissedChanges
            | DomainFailure::FailureRecordUnavailable { .. }
            | DomainFailure::UnknownReadiness => None,
        }
    }

    #[must_use]
    pub fn into_domain_failure(self) -> DomainFailure {
        match self {
            Self::InvalidDomain => DomainFailure::InvalidDomain,
            Self::ClaimRejected => DomainFailure::ClaimRejected,
            Self::ClaimResourceMismatch => DomainFailure::ClaimResourceMismatch,
            Self::StaleClaim => DomainFailure::StaleClaim,
            Self::CertificateUnusable(reason) => DomainFailure::CertificateUnusable(reason),
            Self::CertificateFailed(failure) => DomainFailure::CertificateFailed(failure),
            Self::ServingActivationFailed => DomainFailure::ServingActivationFailed,
            Self::ServingFailed(failure) => DomainFailure::ServingFailed(failure),
        }
    }
}

impl From<DomainStatusFailure> for DomainFailure {
    fn from(failure: DomainStatusFailure) -> Self {
        failure.into_domain_failure()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainReady {
    domain: DomainName,
    certificate: UsableDomainCertificate,
    serving: DomainServingActivation,
}

impl DomainReady {
    #[must_use]
    pub(crate) fn new(
        domain: DomainName,
        certificate: UsableDomainCertificate,
        serving: DomainServingActivation,
    ) -> Self {
        Self {
            domain,
            certificate,
            serving,
        }
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }

    #[must_use]
    pub fn certificate(&self) -> &UsableDomainCertificate {
        &self.certificate
    }

    #[must_use]
    pub fn serving(&self) -> &DomainServingActivation {
        &self.serving
    }

    #[must_use]
    pub fn record(&self) -> DomainReadyRecord {
        DomainReadyRecord::new(
            self.domain.clone(),
            self.certificate.certificate().clone(),
            self.serving.checkpoint().generation(),
        )
        .expect("ready domain and certificate already matched")
    }

    #[must_use]
    pub fn serving_commit(
        &self,
        route: RouteId,
        target: ServingTarget,
        generation: ServingGeneration,
    ) -> ServingCommitRequest {
        ServingCommitRequest::replace_current_route(
            route,
            self.certificate.certificate().hostname.clone(),
            target,
            generation,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainReadyRecord {
    domain: DomainName,
    certificate: CertificateUsability,
    serving_generation: ServingGeneration,
}

impl DomainReadyRecord {
    pub fn new(
        domain: DomainName,
        certificate: CertificateUsability,
        serving_generation: ServingGeneration,
    ) -> Result<Self, DomainFailure> {
        if certificate.hostname.as_str() != domain.as_str() {
            return Err(DomainFailure::CertificateUnusable(
                CertificateUnusableReason::HostnameMismatch,
            ));
        }
        Ok(Self {
            domain,
            certificate,
            serving_generation,
        })
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }

    #[must_use]
    pub fn certificate(&self) -> &CertificateUsability {
        &self.certificate
    }

    #[must_use]
    pub fn serving_generation(&self) -> ServingGeneration {
        self.serving_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableDomainCertificate {
    certificate: CertificateUsability,
}

impl UsableDomainCertificate {
    pub fn new(
        domain: &DomainName,
        certificate: CertificateUsability,
        policy: CertificatePolicy,
    ) -> Result<Self, DomainFailure> {
        if let Some(reason) = certificate_unusable_reason(
            &certificate,
            &domain.https_binding()?,
            policy.minimum_valid_until,
        ) {
            return Err(DomainFailure::CertificateUnusable(reason));
        }
        Ok(Self { certificate })
    }

    #[must_use]
    pub fn certificate(&self) -> &CertificateUsability {
        &self.certificate
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainServingActivation {
    checkpoint: ServingCheckpoint,
}

impl DomainServingActivation {
    #[must_use]
    pub fn active(generation: ServingGeneration) -> Self {
        Self {
            checkpoint: ServingCheckpoint::new(generation),
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_active(generation: ServingGeneration) -> Self {
        Self::active(generation)
    }

    #[must_use]
    pub fn checkpoint(&self) -> &ServingCheckpoint {
        &self.checkpoint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainResource {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainClaim {
    domain: DomainName,
    guard: ClaimGuard<DomainResource>,
}

impl DomainClaim {
    pub fn from_guard(
        domain: DomainName,
        guard: ClaimGuard<DomainResource>,
    ) -> Result<Self, DomainFailure> {
        let expected = domain_resource(&domain)?;
        if guard.resource() != &expected {
            return Err(DomainFailure::ClaimResourceMismatch);
        }
        Ok(Self { domain, guard })
    }

    #[cfg(feature = "test-support")]
    pub fn test_new(
        domain: DomainName,
        resource: TypedResourceId<DomainResource>,
        holder: PrincipalId,
        epoch: FenceEpoch,
        claim_hash: ClaimHash,
        expires_at: SystemTime,
    ) -> Result<Self, DomainFailure> {
        let guard = ClaimGuard::test_new(resource, holder, epoch, claim_hash, expires_at)
            .map_err(|_| DomainFailure::ClaimRejected)?;
        Self::from_guard(domain, guard)
    }

    #[must_use]
    pub fn domain(&self) -> &DomainName {
        &self.domain
    }

    #[must_use]
    pub fn submitted_fence(&self) -> &crate::operation::SubmittedFenceToken {
        self.guard.submitted_fence()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainServingReadiness {
    Active(DomainServingActivation),
    NotActive,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DomainFailure {
    #[error("domain is invalid")]
    InvalidDomain,
    #[error("domain claim was rejected")]
    ClaimRejected,
    #[error("domain claim resource does not match requested domain")]
    ClaimResourceMismatch,
    #[error("domain claim is stale")]
    StaleClaim,
    #[error("certificate is unusable: {0:?}")]
    CertificateUnusable(CertificateUnusableReason),
    #[error("certificate operation failed: {0}")]
    CertificateFailed(CertificateFailure),
    #[error("serving activation failed")]
    ServingActivationFailed,
    #[error("serving operation failed: {0}")]
    ServingFailed(ServingFailure),
    #[error("domain status is unavailable")]
    StatusUnavailable,
    #[error("domain status row payload is invalid")]
    StatusRowsPayloadInvalid,
    #[error("domain status row read timed out")]
    StatusRowsTimeout,
    #[error("domain status row stream was interrupted")]
    StatusRowsStreamInterrupted,
    #[error("domain status row stream missed changes")]
    StatusRowsMissedChanges,
    #[error("{primary} (also failed to record failed domain status: {status})")]
    FailureRecordUnavailable {
        primary: Box<DomainFailure>,
        status: Box<DomainFailure>,
    },
    #[error("domain readiness is unknown")]
    UnknownReadiness,
}

impl DomainFailure {
    #[must_use]
    pub fn primary(&self) -> &Self {
        match self {
            Self::FailureRecordUnavailable { primary, .. } => primary.primary(),
            Self::InvalidDomain
            | Self::ClaimRejected
            | Self::ClaimResourceMismatch
            | Self::StaleClaim
            | Self::CertificateUnusable(_)
            | Self::CertificateFailed(_)
            | Self::ServingActivationFailed
            | Self::ServingFailed(_)
            | Self::StatusUnavailable
            | Self::StatusRowsPayloadInvalid
            | Self::StatusRowsTimeout
            | Self::StatusRowsStreamInterrupted
            | Self::StatusRowsMissedChanges
            | Self::UnknownReadiness => self,
        }
    }
}

pub trait DomainClaimPort {
    fn claim_domain(
        &self,
        context: &MutationContext,
        domain: &DomainName,
    ) -> Result<DomainClaim, DomainFailure>;
}

pub trait DomainCertificatePort {
    fn observe_usable_certificate<'a>(
        &'a self,
        context: &'a MutationContext,
        domain: &'a DomainName,
        policy: CertificatePolicy,
    ) -> impl Future<Output = Result<Option<UsableDomainCertificate>, DomainFailure>> + 'a;

    fn ensure_usable_certificate<'a>(
        &'a self,
        context: &'a MutationContext,
        claim: &'a DomainClaim,
        policy: CertificatePolicy,
    ) -> impl Future<Output = Result<DomainCertificateReadiness, DomainFailure>> + 'a;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainCertificateReadiness {
    Usable(UsableDomainCertificate),
    Pending(DomainPendingReason),
}

impl<T> DomainCertificatePort for T
where
    T: CertificatePort,
{
    async fn observe_usable_certificate(
        &self,
        _context: &MutationContext,
        domain: &DomainName,
        policy: CertificatePolicy,
    ) -> Result<Option<UsableDomainCertificate>, DomainFailure> {
        let binding = domain.https_binding()?;
        match self
            .status(&binding)
            .await
            .map_err(DomainFailure::CertificateFailed)?
        {
            CertificateStatus::Present(certificate) => {
                Ok(UsableDomainCertificate::new(domain, certificate, policy).ok())
            }
            CertificateStatus::Absent
            | CertificateStatus::Unusable(_)
            | CertificateStatus::Unknown => Ok(None),
        }
    }

    async fn ensure_usable_certificate(
        &self,
        context: &MutationContext,
        claim: &DomainClaim,
        policy: CertificatePolicy,
    ) -> Result<DomainCertificateReadiness, DomainFailure> {
        let binding = claim.domain().https_binding()?;
        match self
            .ensure_usable(
                context,
                &binding,
                CertificateDeadline {
                    expires_at: policy.minimum_valid_until,
                },
            )
            .await
            .map_err(DomainFailure::CertificateFailed)?
        {
            EnsureCertificateOutcome::Usable(certificate) => {
                Ok(DomainCertificateReadiness::Usable(
                    UsableDomainCertificate::new(claim.domain(), certificate, policy)?,
                ))
            }
            EnsureCertificateOutcome::Unusable(reason) => {
                Err(DomainFailure::CertificateUnusable(reason))
            }
            EnsureCertificateOutcome::Waiting(reason) => Ok(DomainCertificateReadiness::Pending(
                certificate_wait_to_domain_pending(reason),
            )),
            EnsureCertificateOutcome::FreshnessUnknown => Err(DomainFailure::CertificateUnusable(
                CertificateUnusableReason::FreshnessUnknown,
            )),
        }
    }
}

pub trait DomainServingPort {
    fn activate_certificate(
        &self,
        context: &MutationContext,
        claim: &DomainClaim,
        certificate: &UsableDomainCertificate,
    ) -> Result<DomainServingActivation, DomainFailure>;

    fn verify_certificate_activation(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        certificate: &UsableDomainCertificate,
        serving_generation: ServingGeneration,
    ) -> Result<DomainServingReadiness, DomainFailure>;
}

pub trait DomainStatusPort {
    fn status<'a>(
        &'a self,
        context: &'a MutationContext,
        domain: &'a DomainName,
    ) -> impl Future<Output = Result<DomainStatus, DomainFailure>> + 'a;

    fn record_pending<'a>(
        &'a self,
        context: &'a MutationContext,
        domain: &'a DomainName,
        reason: DomainPendingReason,
    ) -> impl Future<Output = Result<(), DomainFailure>> + 'a;

    fn record_ready<'a>(
        &'a self,
        context: &'a MutationContext,
        ready: DomainReadyRecord,
    ) -> impl Future<Output = Result<(), DomainFailure>> + 'a;

    fn record_failed<'a>(
        &'a self,
        context: &'a MutationContext,
        domain: &'a DomainName,
        failure: DomainStatusFailure,
    ) -> impl Future<Output = Result<(), DomainFailure>> + 'a;
}

fn domain_resource(domain: &DomainName) -> Result<TypedResourceId<DomainResource>, DomainFailure> {
    TypedResourceId::parse(format!("domain:{}", domain.as_str()))
        .map_err(|_| DomainFailure::InvalidDomain)
}

fn certificate_wait_to_domain_pending(reason: CertificateWaitReason) -> DomainPendingReason {
    match reason {
        CertificateWaitReason::IssuanceInProgress => DomainPendingReason::CertificateIssuance,
        CertificateWaitReason::IssuanceInterrupted => {
            DomainPendingReason::CertificateIssuanceInterrupted
        }
    }
}

fn is_valid_domain_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.starts_with('.') || value.ends_with('.') {
        return false;
    }

    value.split('.').all(is_valid_domain_label)
}

fn is_valid_domain_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
        return false;
    }

    label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests;

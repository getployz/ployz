use std::future::Future;

use super::{
    DomainAdd, DomainCertificatePort, DomainCertificateReadiness, DomainClaimPort, DomainFailure,
    DomainPendingReason, DomainReadinessOutcome, DomainReady, DomainServingPort,
    DomainServingReadiness, DomainStatus, DomainStatusPort,
};
use crate::operation::MutationContext;

pub trait DomainReadinessPort {
    fn ensure_ready<'a>(
        &'a self,
        context: &'a MutationContext,
        request: DomainAdd,
    ) -> impl Future<Output = Result<DomainReadinessOutcome, DomainFailure>> + 'a;

    fn verify_ready<'a>(
        &'a self,
        context: &'a MutationContext,
        request: DomainAdd,
    ) -> impl Future<Output = Result<DomainReady, DomainFailure>> + 'a;
}

pub struct DomainReadinessService<C, T, S, R> {
    claims: C,
    certificates: T,
    serving: S,
    records: R,
}

impl<C, T, S, R> DomainReadinessService<C, T, S, R> {
    #[must_use]
    pub fn new(claims: C, certificates: T, serving: S, records: R) -> Self {
        Self {
            claims,
            certificates,
            serving,
            records,
        }
    }
}

impl<C, T, S, R> DomainReadinessPort for DomainReadinessService<C, T, S, R>
where
    C: DomainClaimPort,
    T: DomainCertificatePort,
    S: DomainServingPort,
    R: DomainStatusPort,
{
    async fn ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReadinessOutcome, DomainFailure> {
        DomainReadinessService::ensure_ready(self, context, request).await
    }

    async fn verify_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure> {
        DomainReadinessService::verify_ready(self, context, request).await
    }
}

impl<C, T, S, R> DomainReadinessService<C, T, S, R>
where
    C: DomainClaimPort,
    T: DomainCertificatePort,
    S: DomainServingPort,
    R: DomainStatusPort,
{
    pub async fn ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReadinessOutcome, DomainFailure> {
        let domain = request.domain.clone();
        let result = self.try_ensure_ready(context, request).await;
        match result {
            Ok(outcome) => Ok(outcome),
            Err(primary) => {
                let Some(status_failure) =
                    super::DomainStatusFailure::from_operation_failure(&primary)
                else {
                    return Err(primary);
                };
                match self
                    .records
                    .record_failed(context, &domain, status_failure)
                    .await
                {
                    Ok(()) => Err(primary),
                    Err(status) => Err(DomainFailure::FailureRecordUnavailable {
                        primary: Box::new(primary),
                        status: Box::new(status),
                    }),
                }
            }
        }
    }

    pub async fn verify_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReady, DomainFailure> {
        self.try_reuse_ready(context, &request)
            .await?
            .ok_or(DomainFailure::UnknownReadiness)
    }

    async fn try_ensure_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReadinessOutcome, DomainFailure> {
        if let Some(ready) = self.try_reuse_ready(context, &request).await? {
            return Ok(DomainReadinessOutcome::Ready(ready));
        }

        self.ensure_fresh_ready(context, request).await
    }

    async fn try_reuse_ready(
        &self,
        context: &MutationContext,
        request: &DomainAdd,
    ) -> Result<Option<DomainReady>, DomainFailure> {
        let DomainStatus::Ready(record) = self.records.status(context, &request.domain).await?
        else {
            return Ok(None);
        };
        self.try_upgrade_ready_record(context, &record, request)
            .await
    }

    async fn ensure_fresh_ready(
        &self,
        context: &MutationContext,
        request: DomainAdd,
    ) -> Result<DomainReadinessOutcome, DomainFailure> {
        self.records
            .record_pending(
                context,
                &request.domain,
                DomainPendingReason::CertificateIssuance,
            )
            .await?;

        let domain = request.domain;
        let certificate_policy = request.certificate_policy;
        let claim = self.claims.claim_domain(context, &domain)?;
        let certificate = match self
            .certificates
            .ensure_usable_certificate(context, &claim, certificate_policy)
            .await?
        {
            DomainCertificateReadiness::Usable(certificate) => certificate,
            DomainCertificateReadiness::Pending(reason) => {
                self.records
                    .record_pending(context, claim.domain(), reason.clone())
                    .await?;
                return Ok(DomainReadinessOutcome::Pending(reason));
            }
        };

        self.records
            .record_pending(
                context,
                claim.domain(),
                DomainPendingReason::ServingActivation,
            )
            .await?;
        let serving = self
            .serving
            .activate_certificate(context, &claim, &certificate)?;
        let ready = DomainReady::new(domain, certificate, serving);

        self.records.record_ready(context, ready.record()).await?;
        Ok(DomainReadinessOutcome::Ready(ready))
    }

    async fn try_upgrade_ready_record(
        &self,
        context: &MutationContext,
        record: &super::DomainReadyRecord,
        request: &DomainAdd,
    ) -> Result<Option<DomainReady>, DomainFailure> {
        if record.domain() != &request.domain {
            return Ok(None);
        }
        let Some(certificate) = self
            .certificates
            .observe_usable_certificate(context, &request.domain, request.certificate_policy)
            .await?
        else {
            return Ok(None);
        };
        match self.serving.verify_certificate_activation(
            context,
            &request.domain,
            &certificate,
            record.serving_generation(),
        )? {
            DomainServingReadiness::Active(serving) => Ok(Some(DomainReady::new(
                request.domain.clone(),
                certificate,
                serving,
            ))),
            DomainServingReadiness::NotActive => Ok(None),
        }
    }
}

use crate::domain::{
    DomainFailure, DomainName, DomainPendingReason, DomainReadyRecord, DomainStatus,
    DomainStatusPort,
};
use crate::facts::{
    ProductFact, ProductFactAppendOutcome, ProductFactConflict, ProductFactRejection,
    domain::{
        DOMAIN_FAILED_KIND, DOMAIN_PENDING_KIND, DOMAIN_READY_KIND, DomainFactError,
        DomainStatusEvent, DomainStatusReducer,
    },
};
use crate::operation::{MutationContext, ScopeId};

#[derive(Debug)]
pub(crate) struct PolisDomainStatus<S> {
    scope: ScopeId,
    projection: polis::MemoryProjectionSource<S>,
}

impl<S> PolisDomainStatus<S> {
    #[must_use]
    pub(crate) fn new(scope: ScopeId, projection: polis::MemoryProjectionSource<S>) -> Self {
        Self { scope, projection }
    }
}

impl PolisDomainStatus<polis::MemoryFactStore> {
    #[must_use]
    pub(crate) fn in_memory(scope: ScopeId) -> Self {
        Self::new(
            scope,
            polis::MemoryProjectionSource::new(polis::MemoryFactStore::new()),
        )
    }
}

impl<S> DomainStatusPort for PolisDomainStatus<S>
where
    S: polis::FactStore,
{
    fn status(&self, domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
        self.project_domain_status(domain)
    }

    fn record_pending(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        reason: DomainPendingReason,
    ) -> Result<(), DomainFailure> {
        self.append_domain_event(context, &DomainStatusEvent::pending(domain.clone(), reason))
    }

    fn record_ready(
        &self,
        context: &MutationContext,
        ready: DomainReadyRecord,
    ) -> Result<(), DomainFailure> {
        self.append_domain_event(context, &DomainStatusEvent::ready(ready))
    }

    fn record_failed(
        &self,
        context: &MutationContext,
        domain: &DomainName,
        failure: DomainFailure,
    ) -> Result<(), DomainFailure> {
        self.append_domain_event(context, &DomainStatusEvent::failed(domain.clone(), failure))
    }
}

impl<S> PolisDomainStatus<S>
where
    S: polis::FactStore,
{
    fn append_domain_event(
        &self,
        context: &MutationContext,
        event: &DomainStatusEvent,
    ) -> Result<(), DomainFailure> {
        let envelope = event.encode().map_err(map_domain_fact_error)?;
        let target = super::polis_fact_target(envelope.target()).map_err(map_domain_primitive)?;
        let payload =
            super::polis_fact_payload(envelope.payload()).map_err(map_domain_primitive)?;
        let authority =
            super::context_fact_append_authority(context).map_err(map_domain_primitive)?;
        let grant = polis::FactGrantService::new(DomainFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .map_err(map_domain_polis_error)?;
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!(
                "{}:{}",
                context.operation().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_domain_polis_error)?,
            polis::IdempotencyKey::parse(format!(
                "{}:{}",
                context.idempotency().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_domain_polis_error)?,
            authority,
            grant,
            target,
            payload,
            None,
        );

        match polis::FactStore::append(self.projection.facts(), request)
            .map_err(map_domain_polis_error)
            .and_then(|outcome| {
                super::product_fact_append_outcome(outcome).map_err(map_domain_primitive)
            })? {
            ProductFactAppendOutcome::Appended(_) | ProductFactAppendOutcome::Replayed(_) => Ok(()),
            ProductFactAppendOutcome::Conflict(conflict) => Err(domain_conflict_failure(conflict)),
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized) => {
                Err(DomainFailure::StatusUnavailable)
            }
        }
    }

    fn project_domain_status(&self, domain: &DomainName) -> Result<DomainStatus, DomainFailure> {
        let request = polis::ProjectionRequest::new(
            domain_projection_view(domain)?,
            domain_fact_query(&self.scope, domain)?,
            PolisDomainReducer {
                reducer: DomainStatusReducer::new(domain.clone()),
            },
        );
        polis::ProjectionSource::project(&self.projection, request)
            .map(|snapshot| snapshot.into_view())
            .map_err(map_domain_projection_error)
    }
}

struct PolisDomainReducer {
    reducer: DomainStatusReducer,
}

impl polis::FactReducer for PolisDomainReducer {
    type View = DomainStatus;
    type Error = DomainFailure;

    fn reduce(
        &self,
        facts: Vec<polis::VerifiedFact>,
    ) -> std::result::Result<Self::View, Self::Error> {
        let mut events = Vec::with_capacity(facts.len());
        for fact in facts {
            let decoded = DomainStatusEvent::decode_payload(fact.payload().as_bytes())
                .map_err(map_domain_fact_error)?;
            let expected_target = decoded
                .encode()
                .map_err(map_domain_fact_error)?
                .target()
                .clone();
            let actual_target = super::product_fact_receipt(fact.receipt())
                .map_err(map_domain_primitive)?
                .target()
                .clone();
            if actual_target != expected_target {
                return Err(DomainFailure::StatusUnavailable);
            }
            events.push(decoded);
        }
        self.reducer.reduce(events).map_err(map_domain_fact_error)
    }
}

struct DomainFactGrantAuthority;

impl polis::FactGrantAuthority for DomainFactGrantAuthority {
    fn decide(
        &self,
        _authority: &polis::AuthorityContext,
        target: &polis::FactTarget,
        purpose: polis::FactGrantPurpose,
    ) -> polis::FactGrantDecision {
        match purpose {
            polis::FactGrantPurpose::Append if domain_fact_target_allowed(target) => {
                polis::FactGrantDecision::allowed()
            }
            polis::FactGrantPurpose::Append | polis::FactGrantPurpose::ReplicaImport => {
                polis::FactGrantDecision::denied()
            }
        }
    }
}

fn domain_fact_target_allowed(target: &polis::FactTarget) -> bool {
    target.resource().as_str().starts_with("domain-status:")
        && matches!(
            target.kind().as_str(),
            DOMAIN_PENDING_KIND | DOMAIN_READY_KIND | DOMAIN_FAILED_KIND
        )
}

fn domain_projection_view(domain: &DomainName) -> Result<polis::ProjectionView, DomainFailure> {
    polis::ProjectionKey::parse(format!("ployz.domain.status:{}", domain.as_str()))
        .map(polis::ProjectionView::new)
        .map_err(map_domain_polis_error)
}

fn domain_fact_query(
    scope: &ScopeId,
    domain: &DomainName,
) -> Result<polis::FactQuery, DomainFailure> {
    let scope = polis::ScopeId::parse(scope.as_str()).map_err(map_domain_polis_error)?;
    let resource = polis::ResourceId::parse(format!("domain-status:{}", domain.as_str()))
        .map_err(map_domain_polis_error)?;
    Ok(polis::FactQuery::new(scope).resource(resource))
}

fn domain_conflict_failure(_conflict: ProductFactConflict) -> DomainFailure {
    DomainFailure::StatusUnavailable
}

fn map_domain_projection_error(error: polis::ProjectionError<DomainFailure>) -> DomainFailure {
    match error {
        polis::ProjectionError::Source(source) => map_domain_polis_error(source),
        polis::ProjectionError::SubstrateFailed { .. } => DomainFailure::StatusUnavailable,
        polis::ProjectionError::Reducer(failure) => failure,
    }
}

fn map_domain_primitive(_failure: crate::error::PrimitiveFailure) -> DomainFailure {
    DomainFailure::StatusUnavailable
}

fn map_domain_polis_error(error: polis::Error) -> DomainFailure {
    map_domain_primitive(super::map_polis_error(error))
}

fn map_domain_fact_error(_error: DomainFactError) -> DomainFailure {
    DomainFailure::StatusUnavailable
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use crate::acme::{
        CertificateActivation, CertificateMaterialState, CertificateUsability, Hostname,
        RevocationFreshness,
    };
    use crate::domain::{DomainFailure, DomainName, DomainPendingReason, DomainReadyRecord};
    use crate::error::CertificateFailure;
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
        PrincipalId, ScopeId,
    };
    use crate::serving::ServingGeneration;

    use super::*;

    #[test]
    fn projects_recorded_ready_status() {
        let records = status_records();
        let ready = ready_record();

        records
            .record_pending(
                &context(),
                ready.domain(),
                DomainPendingReason::CertificateIssuance,
            )
            .expect("pending");
        records
            .record_ready(&context(), ready.clone())
            .expect("ready");

        assert_eq!(
            records.status(&domain()).expect("status"),
            DomainStatus::Ready(ready)
        );
    }

    #[test]
    fn projects_latest_failure_status() {
        let records = status_records();

        records
            .record_pending(
                &context(),
                &domain(),
                DomainPendingReason::CertificateIssuance,
            )
            .expect("pending");
        records
            .record_failed(
                &context(),
                &domain(),
                DomainFailure::CertificateFailed(CertificateFailure::IssuanceFailed),
            )
            .expect("failed");

        assert_eq!(
            records.status(&domain()).expect("status"),
            DomainStatus::Failed(DomainFailure::CertificateFailed(
                CertificateFailure::IssuanceFailed
            ))
        );
    }

    #[test]
    fn projects_unknown_for_domains_without_records() {
        let records = status_records();

        records
            .record_pending(
                &context(),
                &domain(),
                DomainPendingReason::CertificateIssuance,
            )
            .expect("pending");

        assert_eq!(
            records
                .status(&DomainName::parse("other.example.com").expect("domain"))
                .expect("status"),
            DomainStatus::Unknown
        );
    }

    fn status_records() -> PolisDomainStatus<polis::MemoryFactStore> {
        PolisDomainStatus::in_memory(ScopeId::parse("cluster").expect("scope"))
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("deploy-1").expect("operation"),
            IdempotencyKey::parse("deploy-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn ready_record() -> DomainReadyRecord {
        DomainReadyRecord::new(
            domain(),
            CertificateUsability {
                hostname: Hostname::parse("app.example.com").expect("hostname"),
                not_after: UNIX_EPOCH + Duration::from_secs(86_400),
                activation: CertificateActivation::Acknowledged,
                material: CertificateMaterialState::PresentProtected,
                revocation: RevocationFreshness::KnownFresh,
            },
            ServingGeneration::new(7),
        )
        .expect("ready")
    }

    fn domain() -> DomainName {
        DomainName::parse("app.example.com").expect("domain")
    }
}

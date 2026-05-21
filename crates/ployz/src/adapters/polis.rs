//! Polis adapter helpers for Ployz composition code.

use crate::error::PrimitiveFailure;
use crate::error::ProjectionFailure;
use crate::projection::{
    ProductGrantEpoch, ProductPrincipalId, ProductRecordEnvelope, ProductRecordMetadata,
    ProductScopeId, SourceWatermark,
};

#[must_use]
pub fn map_polis_error(error: polis::Error) -> PrimitiveFailure {
    match error {
        polis::Error::Unauthorized => PrimitiveFailure::Unauthorized,
        polis::Error::Conflict => PrimitiveFailure::Conflict,
        polis::Error::Timeout => PrimitiveFailure::Timeout,
        polis::Error::StaleFence => PrimitiveFailure::StaleFence,
        polis::Error::NoResponder => PrimitiveFailure::NoResponder,
        polis::Error::FreshnessUnknown => PrimitiveFailure::FreshnessUnknown,
        polis::Error::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::Error::TerminalAlreadyWritten => PrimitiveFailure::TerminalAlreadyWritten,
    }
}

pub fn product_record_from_polis(
    record: polis::records::AuthorizedRecord,
) -> Result<ProductRecordEnvelope, ProjectionFailure> {
    let proof = record.proof();
    let metadata = ProductRecordMetadata::new(
        ProductPrincipalId::parse(proof.principal().as_str())?,
        ProductScopeId::parse(proof.scope().as_str())?,
        ProductGrantEpoch::new(proof.grant_epoch().value()),
        SourceWatermark::new(proof.source_watermark().value()),
        proof.schema_version().value(),
    );
    Ok(ProductRecordEnvelope::new(record.into_payload(), metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_terminal_conflict_without_display_parsing() {
        assert_eq!(
            map_polis_error(polis::Error::TerminalAlreadyWritten),
            PrimitiveFailure::TerminalAlreadyWritten
        );
    }

    #[test]
    fn maps_authorized_record_without_product_imports_in_polis() {
        struct AllowAuthority;

        impl polis::Authority for AllowAuthority {
            fn decide(
                &self,
                _principal: &polis::PrincipalId,
                _scope: &polis::ScopeId,
            ) -> polis::AuthorityDecision {
                polis::AuthorityDecision::allowed(polis::GrantEpoch::new(2))
            }
        }

        let principal = polis::PrincipalId::parse("node-a").expect("principal");
        let scope = polis::ScopeId::parse("cluster").expect("scope");
        let authority = polis::AuthorityService::new(AllowAuthority)
            .authorize::<()>(principal, scope)
            .expect("authorized");
        let record = polis::records::AuthorizedRecord::new(
            vec![1, 2, 3],
            &authority,
            polis::SourceWatermark::new(5),
            polis::records::SchemaVersion::new(1),
        )
        .expect("record");

        let product_record = product_record_from_polis(record).expect("product record");

        assert_eq!(product_record.metadata().grant_epoch().value(), 2);
    }
}

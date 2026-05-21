//! Polis adapter helpers for Ployz composition code.

use crate::error::PrimitiveFailure;
use crate::error::ProjectionFailure;
use crate::projection::{
    ProductGrantEpoch, ProductPrincipalId, ProductProofMetadata, ProductRecordEnvelope,
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
    Ok(ProductRecordEnvelope {
        payload: record.payload,
        proof: ProductProofMetadata {
            principal: ProductPrincipalId::parse(record.proof.principal.as_str())?,
            scope: ProductScopeId::parse(record.proof.scope.as_str())?,
            grant_epoch: ProductGrantEpoch::new(record.proof.grant_epoch.value()),
            source_watermark: SourceWatermark::new(record.proof.source_watermark.value()),
            schema_version: record.proof.schema_version.value(),
        },
    })
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
        let principal = polis::PrincipalId::parse("node-a").expect("principal");
        let scope = polis::ScopeId::parse("cluster").expect("scope");
        let authority = polis::AuthorityContext::new(principal, scope, polis::GrantEpoch::new(2));
        let proof = polis::records::ProofMetadata::new(
            authority,
            polis::SourceWatermark::new(5),
            polis::records::SchemaVersion::new(1),
        );
        let record = polis::records::AuthorizedRecord::new(vec![1, 2, 3], proof);

        let product_record = product_record_from_polis(record).expect("product record");

        assert_eq!(product_record.proof.grant_epoch.value(), 2);
    }
}

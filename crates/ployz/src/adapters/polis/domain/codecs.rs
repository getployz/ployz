use crate::acme::{
    CertificateActivation, CertificateMaterialState, CertificateUsability, Hostname,
    RevocationFreshness,
};
use crate::domain::{DomainFailure, DomainName, DomainPendingReason};
use crate::operation::{IdempotencyKey, MutationWriteIdentity, OperationId, ScopeId};
use crate::serving::ServingGeneration;
use serde::{Deserialize, Serialize};

use super::{DomainStatusKind, DomainStatusRow, StoredDomainStatus};
use crate::adapters::polis::failure_codecs;
use crate::adapters::polis::scalars;

pub(super) fn decode_domain_status_row(
    row: &polis::StoreRow,
) -> Result<DomainStatusRow, polis::StoreError> {
    let domain =
        DomainName::parse(row.text("domain")?).map_err(|_| polis::StoreError::MalformedPayload)?;
    let status_kind = domain_status_kind_from_tag(row.text("status")?)
        .ok_or(polis::StoreError::MalformedPayload)?;
    let status = status_from_payload(status_kind, &domain, row.text("status_payload")?)?;
    Ok(DomainStatusRow {
        domain,
        status,
        write: MutationWriteIdentity::new(
            ScopeId::parse(row.text("scope")?).map_err(|_| polis::StoreError::MalformedPayload)?,
            OperationId::parse(row.text("operation")?)
                .map_err(|_| polis::StoreError::MalformedPayload)?,
            IdempotencyKey::parse(row.text("idempotency")?)
                .map_err(|_| polis::StoreError::MalformedPayload)?,
        ),
    })
}

pub(super) fn status_payload(status: &StoredDomainStatus) -> Result<String, polis::StoreError> {
    match status {
        StoredDomainStatus::Pending(reason) => serde_json::to_string(&PersistedPendingStatus {
            reason: pending_reason_tag(reason).to_string(),
        })
        .map_err(|_| polis::StoreError::MalformedPayload),
        StoredDomainStatus::Ready {
            certificate,
            serving_generation,
        } => {
            let (certificate_not_after_secs, certificate_not_after_nanos) =
                scalars::unix_time_parts(certificate.not_after)?;
            serde_json::to_string(&PersistedReadyStatus {
                certificate_not_after_secs,
                certificate_not_after_nanos,
                certificate_activation: certificate_activation_tag(&certificate.activation)
                    .to_string(),
                certificate_material: certificate_material_tag(&certificate.material).to_string(),
                certificate_revocation: revocation_freshness_tag(&certificate.revocation)
                    .to_string(),
                serving_generation: scalars::i64_from_u64(serving_generation.value())?,
            })
            .map_err(|_| polis::StoreError::MalformedPayload)
        }
        StoredDomainStatus::Failed(failure) => serde_json::to_string(&PersistedFailedStatus {
            failure_payload: failure_codecs::domain_status_failure_payload(failure)?,
        })
        .map_err(|_| polis::StoreError::MalformedPayload),
    }
}

fn status_from_payload(
    status_kind: DomainStatusKind,
    domain: &DomainName,
    payload: &str,
) -> Result<StoredDomainStatus, polis::StoreError> {
    match status_kind {
        DomainStatusKind::Pending => {
            let payload = serde_json::from_str::<PersistedPendingStatus>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(StoredDomainStatus::Pending(
                pending_reason_from_tag(&payload.reason)
                    .map_err(|_| polis::StoreError::MalformedPayload)?,
            ))
        }
        DomainStatusKind::Ready => {
            let payload = serde_json::from_str::<PersistedReadyStatus>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(StoredDomainStatus::Ready {
                certificate: CertificateUsability {
                    hostname: Hostname::parse(domain.as_str())
                        .map_err(|_| polis::StoreError::MalformedPayload)?,
                    not_after: scalars::system_time_from_unix_parts(
                        payload.certificate_not_after_secs,
                        payload.certificate_not_after_nanos,
                    )?,
                    activation: certificate_activation_from_tag(&payload.certificate_activation)
                        .map_err(|_| polis::StoreError::MalformedPayload)?,
                    material: certificate_material_from_tag(&payload.certificate_material)
                        .map_err(|_| polis::StoreError::MalformedPayload)?,
                    revocation: revocation_freshness_from_tag(&payload.certificate_revocation)
                        .map_err(|_| polis::StoreError::MalformedPayload)?,
                },
                serving_generation: ServingGeneration::new(scalars::u64_from_i64(
                    payload.serving_generation,
                )?),
            })
        }
        DomainStatusKind::Failed => {
            let payload = serde_json::from_str::<PersistedFailedStatus>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(StoredDomainStatus::Failed(
                failure_codecs::domain_status_failure_from_payload(&payload.failure_payload)?,
            ))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPendingStatus {
    reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedReadyStatus {
    certificate_not_after_secs: i64,
    certificate_not_after_nanos: i64,
    certificate_activation: String,
    certificate_material: String,
    certificate_revocation: String,
    serving_generation: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFailedStatus {
    failure_payload: String,
}

pub(super) fn domain_status_kind_tag(status: DomainStatusKind) -> &'static str {
    match status {
        DomainStatusKind::Pending => "pending",
        DomainStatusKind::Ready => "ready",
        DomainStatusKind::Failed => "failed",
    }
}

fn domain_status_kind_from_tag(tag: &str) -> Option<DomainStatusKind> {
    match tag {
        "pending" => Some(DomainStatusKind::Pending),
        "ready" => Some(DomainStatusKind::Ready),
        "failed" => Some(DomainStatusKind::Failed),
        _ => None,
    }
}

pub(super) fn pending_reason_tag(reason: &DomainPendingReason) -> &'static str {
    match reason {
        DomainPendingReason::CertificateIssuance => "certificate_issuance",
        DomainPendingReason::CertificateIssuanceInterrupted => "certificate_issuance_interrupted",
        DomainPendingReason::ServingActivation => "serving_activation",
    }
}

fn pending_reason_from_tag(tag: &str) -> Result<DomainPendingReason, DomainFailure> {
    match tag {
        "certificate_issuance" => Ok(DomainPendingReason::CertificateIssuance),
        "certificate_issuance_interrupted" => {
            Ok(DomainPendingReason::CertificateIssuanceInterrupted)
        }
        "serving_activation" => Ok(DomainPendingReason::ServingActivation),
        _ => Err(DomainFailure::StatusUnavailable),
    }
}

pub(super) fn certificate_activation_tag(activation: &CertificateActivation) -> &'static str {
    match activation {
        CertificateActivation::Acknowledged => "acknowledged",
        CertificateActivation::Rejected => "rejected",
        CertificateActivation::Pending => "pending",
        CertificateActivation::Unknown => "unknown",
    }
}

fn certificate_activation_from_tag(tag: &str) -> Result<CertificateActivation, DomainFailure> {
    match tag {
        "acknowledged" => Ok(CertificateActivation::Acknowledged),
        "rejected" => Ok(CertificateActivation::Rejected),
        "pending" => Ok(CertificateActivation::Pending),
        "unknown" => Ok(CertificateActivation::Unknown),
        _ => Err(DomainFailure::StatusUnavailable),
    }
}

pub(super) fn certificate_material_tag(material: &CertificateMaterialState) -> &'static str {
    match material {
        CertificateMaterialState::PresentProtected => "present_protected",
        CertificateMaterialState::Missing => "missing",
        CertificateMaterialState::Unsafe => "unsafe",
    }
}

fn certificate_material_from_tag(tag: &str) -> Result<CertificateMaterialState, DomainFailure> {
    match tag {
        "present_protected" => Ok(CertificateMaterialState::PresentProtected),
        "missing" => Ok(CertificateMaterialState::Missing),
        "unsafe" => Ok(CertificateMaterialState::Unsafe),
        _ => Err(DomainFailure::StatusUnavailable),
    }
}

pub(super) fn revocation_freshness_tag(revocation: &RevocationFreshness) -> &'static str {
    match revocation {
        RevocationFreshness::KnownFresh => "known_fresh",
        RevocationFreshness::KnownRevoked => "known_revoked",
        RevocationFreshness::Unknown => "unknown",
    }
}

fn revocation_freshness_from_tag(tag: &str) -> Result<RevocationFreshness, DomainFailure> {
    match tag {
        "known_fresh" => Ok(RevocationFreshness::KnownFresh),
        "known_revoked" => Ok(RevocationFreshness::KnownRevoked),
        "unknown" => Ok(RevocationFreshness::Unknown),
        _ => Err(DomainFailure::StatusUnavailable),
    }
}

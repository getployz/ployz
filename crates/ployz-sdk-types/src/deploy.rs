use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployReserveRequest {
    pub namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployReserved {
    pub reservation_id: DeployReservationId,
    pub expires_at: DeployReservationExpiresAt,
}

pub type DeployReserveResponse = OperationApiResponse<DeployReserved, DeployReserveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum DeployReserveError {
    #[error("deploy reservation unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployPreviewRequest {
    pub target: DeployPreviewTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployPreview {
    pub projection: DeployPreviewProjection,
    pub build_platform_requirements: BTreeMap<ServiceId, BuildPlatforms>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unusable_machines: Vec<UnusableMachine>,
}

pub type DeployPreviewResponse = OperationApiResponse<DeployPreview, DeployPreviewError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPreviewError {
    #[error("deploy preview target invalid: {message}")]
    InvalidTarget { message: FailureMessage },
    #[error("deploy preview planning failed: {message}")]
    PlanningFailed {
        message: FailureMessage,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unusable_machines: Vec<UnusableMachine>,
    },
    #[error("deploy preview unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub idempotency_key: OperationIdempotencyKey,
    pub reservation_id: DeployReservationId,
    pub target: DeployRequest,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
}

pub type DeploySubmitResponse = OperationApiResponse<AcceptedOperation, DeploySubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum DeploySubmitError {
    #[error(
        "deploy reservation {} was not issued for namespace {}",
        .reservation_id.get(),
        .namespace_id.as_str()
    )]
    ReservationNotFound {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
    },
    #[error(
        "deploy reservation {} expired at unix second {}",
        .reservation_id.get(),
        .expired_at.unix_seconds()
    )]
    ReservationExpired {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        expired_at: DeployReservationExpiresAt,
    },
    #[error(
        "deploy reservation {} is stale; namespace {} committed newer reservation {}; rerun deploy to supersede deliberately",
        .reservation_id.get(),
        .namespace_id.as_str(),
        .last_committed_reservation_id.get()
    )]
    StaleReservation {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        last_committed_reservation_id: DeployReservationId,
    },
    #[error(
        "deploy reservation {} was already committed by operation {}",
        .reservation_id.get(),
        .owner_operation_id.as_str()
    )]
    ReservationAlreadyCommitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        owner_operation_id: OperationId,
    },
    #[error("deploy target invalid for operation {}: {message}", .operation_id.as_str())]
    InvalidTarget {
        operation_id: OperationId,
        message: FailureMessage,
    },
    #[error(
        "namespace {} is busy with operation {}",
        .namespace_id.as_str(),
        .owner_operation_id.as_str()
    )]
    ResourceBusy {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        owner_operation_id: OperationId,
    },
    #[error("deploy submit {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[cfg(test)]
mod tests {
    #[test]
    fn authoritative_deploy_rejects_pending_build_input() {
        let request = super::DeploySubmitRequest {
            idempotency_key: ployz_core::operation::OperationIdempotencyKey::try_new(
                "idem_preview",
            )
            .expect("idempotency key"),
            reservation_id: ployz_core::deploy::DeployReservationId::first(),
            target: ployz_core::deploy::DeployRequest {
                namespace_id: ployz_core::ids::NamespaceId::try_new("default")
                    .expect("namespace id"),
                origin: None,
                volumes: std::collections::BTreeMap::new(),
                services: Vec::new(),
            },
            registry_credentials: std::collections::BTreeMap::new(),
        };
        let mut request = serde_json::to_value(request).expect("deploy request serializes");
        request
            .as_object_mut()
            .expect("deploy request is an object")
            .insert("pending_builds".to_owned(), serde_json::json!(["api"]));

        assert!(serde_json::from_value::<super::DeploySubmitRequest>(request).is_err());
    }
}

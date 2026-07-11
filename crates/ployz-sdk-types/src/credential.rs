use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CredentialAddRequest {
    pub operation_id: OperationId,
    pub grant: CredentialGrant,
}

pub type CredentialAddResponse = OperationApiResponse<AcceptedOperation, CredentialAddError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CredentialRemoveRequest {
    pub operation_id: OperationId,
    pub public_key: NatsUserPublicKey,
}

pub type CredentialRemoveResponse = OperationApiResponse<AcceptedOperation, CredentialRemoveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CredentialListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CredentialListResult {
    pub credentials: Vec<CredentialGrant>,
}

pub type CredentialListResponse = OperationApiResponse<CredentialListResult, CredentialListError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialAddError {
    #[error("credential add {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialRemoveError {
    #[error("credential remove {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialListError {
    #[error("credential list unavailable: {message}")]
    Unavailable { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_add_request_rejects_blank_names_at_the_wire_boundary() {
        let value = serde_json::json!({
            "operation_id": "op_credential_add",
            "grant": {
                "public_key": "UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4",
                "name": "   ",
                "role": "operator"
            }
        });

        assert!(serde_json::from_value::<CredentialAddRequest>(value).is_err());
    }

    #[test]
    fn credential_list_result_preserves_non_unique_names() {
        let credentials = [
            ployz_core::nats_config::MintedNatsUser::generate()
                .expect("first user key")
                .public,
            ployz_core::nats_config::MintedNatsUser::generate()
                .expect("second user key")
                .public,
        ]
        .into_iter()
        .map(|public_key| CredentialGrant {
            public_key,
            name: CredentialName::try_new("Ployz Cloud").expect("credential name"),
            role: CredentialRole::Operator,
        })
        .collect();

        let result = CredentialListResult { credentials };

        assert_eq!(result.credentials.len(), 2);
    }
}

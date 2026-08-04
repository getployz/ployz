use serde::{Deserialize, Serialize};

use crate::ids::OperationId;
use crate::ingress::IngressConfiguration;
use crate::operation::EventSequence;

use super::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressConfigureRequest {
    pub operation_id: OperationId,
    pub configuration: IngressConfiguration,
}

pub type IngressConfigureResponse = OperationApiResponse<AcceptedOperation, IngressConfigureError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressConfigureError {
    #[error("invalid ingress configuration: {message}")]
    InvalidConfiguration { message: String },
    #[error("ingress configuration is busy with operation {}", .owner.as_str())]
    ResourceBusy { owner: OperationId },
    #[error("ingress configuration {} unavailable: {message}", .operation_id.as_str())]
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
    use super::*;

    #[test]
    fn request_rejects_ployz_hostnames_without_dns_target() {
        let request = serde_json::json!({
            "operation_id": "op_ingress_configure",
            "configuration": {
                "automatic_hostnames": { "mode": "ployz" },
                "ployz_dns_target": "disabled"
            }
        });

        assert!(serde_json::from_value::<IngressConfigureRequest>(request).is_err());
    }
}

use std::collections::BTreeSet;

use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{CapabilityName, MachineId};

pub const PROTOCOL_MAJOR: u32 = 1;
pub const DESCRIBE_CONTRACT_CAPABILITY: &str = "ployz.rpc.describe-contract.v1";
pub const RESET_MACHINE_CAPABILITY: &str = "ployz.machine.reset.v1";

/// The only protobuf-shaped value understood by tonic and the transparent proxy.
#[derive(Clone, PartialEq, Message)]
pub struct OpaquePayload {
    #[prost(bytes = "vec", tag = "1")]
    pub json: Vec<u8>,
}

/// Generated tonic client and server for the shared Machine service.
pub mod transport {
    include!(concat!(env!("OUT_DIR"), "/ployz.rpc.v1.MachineRpc.rs"));
}

pub use transport::{
    machine_rpc_client::MachineRpcClient, machine_rpc_server::MachineRpc,
    machine_rpc_server::MachineRpcServer,
};

impl OpaquePayload {
    #[must_use]
    pub fn new(json: Vec<u8>) -> Self {
        Self { json }
    }

    pub fn from_json<T: Serialize>(value: &T) -> Result<Self, CodecError> {
        serde_json::to_vec(value)
            .map(Self::new)
            .map_err(CodecError::EncodeJson)
    }

    pub fn decode_json<T: DeserializeOwned>(&self) -> Result<T, CodecError> {
        serde_json::from_slice(&self.json).map_err(CodecError::DecodeJson)
    }

    pub fn decode_request(&self) -> Result<RpcRequest, CodecError> {
        let header: RequestHeader = self.decode_json()?;
        validate_protocol_major(header.protocol_major)?;
        if !matches!(header.command.as_str(), "describe_contract" | "reset") {
            return Err(CodecError::UnsupportedCommand(header.command));
        }
        self.decode_json()
    }

    pub fn decode_response(&self) -> Result<RpcResponse, CodecError> {
        let response: RpcResponse = self.decode_json()?;
        validate_protocol_major(response.protocol_major)?;
        Ok(response)
    }
}

fn validate_protocol_major(requested: u32) -> Result<(), CodecError> {
    if requested == PROTOCOL_MAJOR {
        Ok(())
    } else {
        Err(CodecError::UnsupportedProtocolMajor {
            requested,
            supported: PROTOCOL_MAJOR,
        })
    }
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("could not encode JSON payload: {0}")]
    EncodeJson(serde_json::Error),
    #[error("could not decode JSON payload: {0}")]
    DecodeJson(serde_json::Error),
    #[error("unsupported RPC command {0:?}")]
    UnsupportedCommand(String),
    #[error("unsupported protocol major {requested}; this endpoint supports {supported}")]
    UnsupportedProtocolMajor { requested: u32, supported: u32 },
    #[error("expected response kind {expected:?}, received {actual:?}")]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },
}

/// The empty payload of the capability-description command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DescribeContractRequest {}

/// The empty payload of the reset command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResetRequest {}

/// Commands are closed and own their typed payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command", content = "payload")]
pub enum RpcRequestBody {
    DescribeContract(DescribeContractRequest),
    Reset(ResetRequest),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub protocol_major: u32,
    #[serde(flatten)]
    pub body: RpcRequestBody,
}

impl RpcRequest {
    #[must_use]
    pub fn describe_contract() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::DescribeContract(DescribeContractRequest {}),
        }
    }

    #[must_use]
    pub fn reset() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Reset(ResetRequest {}),
        }
    }

    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }
}

#[derive(Deserialize)]
struct RequestHeader {
    protocol_major: u32,
    command: String,
}

crate::value::open_string_enum!(ResponseKind, Unknown {
    ContractDescription => "contract_description",
    ResetAccepted => "reset_accepted",
    Error => "error",
});

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResetAccepted {}

/// Known responses own typed payloads; future responses retain their raw value.
#[derive(Clone, Debug, PartialEq)]
pub enum RpcResponseBody {
    ContractDescription(ContractDescription),
    ResetAccepted(ResetAccepted),
    Error(RpcError),
    Unknown { kind: String, payload: Value },
}

impl RpcResponseBody {
    #[must_use]
    pub fn kind(&self) -> ResponseKind {
        match self {
            Self::ContractDescription(_) => ResponseKind::ContractDescription,
            Self::ResetAccepted(_) => ResponseKind::ResetAccepted,
            Self::Error(_) => ResponseKind::Error,
            Self::Unknown { kind, .. } => ResponseKind::Unknown(kind.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcResponse {
    pub protocol_major: u32,
    pub body: RpcResponseBody,
}

impl RpcResponse {
    #[must_use]
    pub fn contract_description(description: ContractDescription) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContractDescription(description),
        }
    }

    #[must_use]
    pub fn error(error: RpcError) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::Error(error),
        }
    }

    #[must_use]
    pub fn reset_accepted() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ResetAccepted(ResetAccepted {}),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ResponseKind {
        self.body.kind()
    }

    pub fn decode_contract_description(&self) -> Result<&ContractDescription, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        match &self.body {
            RpcResponseBody::ContractDescription(description) => Ok(description),
            body @ (RpcResponseBody::ResetAccepted(_)
            | RpcResponseBody::Error(_)
            | RpcResponseBody::Unknown { .. }) => Err(CodecError::UnexpectedResponse {
                expected: "contract_description",
                actual: body.kind().as_str().to_owned(),
            }),
        }
    }

    pub fn decode_reset_accepted(&self) -> Result<(), CodecError> {
        validate_protocol_major(self.protocol_major)?;
        match &self.body {
            RpcResponseBody::ResetAccepted(_) => Ok(()),
            body @ (RpcResponseBody::ContractDescription(_)
            | RpcResponseBody::Error(_)
            | RpcResponseBody::Unknown { .. }) => Err(CodecError::UnexpectedResponse {
                expected: "reset_accepted",
                actual: body.kind().as_str().to_owned(),
            }),
        }
    }

    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }
}

#[derive(Serialize, Deserialize)]
struct WireResponse {
    protocol_major: u32,
    kind: ResponseKind,
    #[serde(default)]
    payload: Value,
}

impl Serialize for RpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let payload = match &self.body {
            RpcResponseBody::ContractDescription(description) => {
                serde_json::to_value(description).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ResetAccepted(accepted) => {
                serde_json::to_value(accepted).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Error(error) => {
                serde_json::to_value(error).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Unknown { payload, .. } => payload.clone(),
        };
        WireResponse {
            protocol_major: self.protocol_major,
            kind: self.kind(),
            payload,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireResponse::deserialize(deserializer)?;
        let body = match wire.kind {
            ResponseKind::ContractDescription => RpcResponseBody::ContractDescription(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::ResetAccepted => RpcResponseBody::ResetAccepted(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Error => RpcResponseBody::Error(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Unknown(kind) => RpcResponseBody::Unknown {
                kind,
                payload: wire.payload,
            },
        };
        Ok(Self {
            protocol_major: wire.protocol_major,
            body,
        })
    }
}

/// The capabilities currently advertised by one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractDescription {
    pub machine_id: MachineId,
    pub protocol_major: u32,
    /// Diagnostic only. Callers select behavior using capability names.
    pub daemon_version: String,
    #[serde(default)]
    pub capabilities: BTreeSet<CapabilityName>,
}

impl ContractDescription {
    #[must_use]
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|advertised| advertised.as_str() == capability)
    }
}

crate::value::open_string_enum!(RpcErrorCode, Unknown {
    InvalidArgument => "invalid_argument",
    NotFound => "not_found",
    Ambiguous => "ambiguous",
    Unsupported => "unsupported",
    Unavailable => "unavailable",
    Conflict => "conflict",
    Internal => "internal",
});

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

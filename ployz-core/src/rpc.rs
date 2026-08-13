use std::collections::BTreeSet;

use prost::{Message, Oneof};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{CapabilityName, MachineId, MachineName, ValueError};

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

/// Schema-blind response wrapper used only when one request fans out to several Machines.
#[derive(Clone, PartialEq, Message)]
pub struct FanoutResponse {
    #[prost(string, tag = "1")]
    machine_id: String,
    #[prost(string, tag = "2")]
    machine_name: String,
    #[prost(oneof = "FanoutOutcome", tags = "3, 4")]
    pub outcome: Option<FanoutOutcome>,
}

#[derive(Clone, Eq, PartialEq, Oneof)]
pub enum FanoutOutcome {
    /// One complete gRPC message frame, including its compression byte and length.
    #[prost(bytes, tag = "3")]
    FramedPayload(Vec<u8>),
    #[prost(message, tag = "4")]
    Failure(FanoutFailure),
}

#[derive(Clone, Eq, PartialEq, Message)]
pub struct FanoutFailure {
    #[prost(uint32, tag = "1")]
    pub code: u32,
    #[prost(string, tag = "2")]
    pub message: String,
    #[prost(bytes, tag = "3")]
    pub details: Vec<u8>,
}

impl FanoutResponse {
    pub fn success(
        machine_id: &MachineId,
        machine_name: &MachineName,
        framed_payload: Vec<u8>,
    ) -> Result<Self, FramingError> {
        require_one_frame(&framed_payload)?;
        Ok(Self {
            machine_id: machine_id.to_string(),
            machine_name: machine_name.to_string(),
            outcome: Some(FanoutOutcome::FramedPayload(framed_payload)),
        })
    }

    #[must_use]
    pub fn failure(
        machine_id: &MachineId,
        machine_name: &MachineName,
        failure: FanoutFailure,
    ) -> Self {
        Self {
            machine_id: machine_id.to_string(),
            machine_name: machine_name.to_string(),
            outcome: Some(FanoutOutcome::Failure(failure)),
        }
    }

    pub fn machine_id(&self) -> Result<MachineId, ValueError> {
        MachineId::parse(self.machine_id.clone())
    }

    pub fn machine_name(&self) -> Result<MachineName, ValueError> {
        MachineName::parse(self.machine_name.clone())
    }

    #[must_use]
    pub fn encode_grpc_frame(&self) -> Vec<u8> {
        encode_grpc_frame(&self.encode_to_vec())
    }

    pub fn decode_grpc_frame(frame: &[u8]) -> Result<Self, FramingError> {
        require_one_frame(frame)?;
        Self::decode(
            frame
                .get(GRPC_FRAME_HEADER_LEN..)
                .expect("one complete gRPC frame was checked"),
        )
        .map_err(|error| FramingError::InvalidEnvelope(error.to_string()))
    }
}

pub const GRPC_FRAME_HEADER_LEN: usize = 5;

#[must_use]
pub fn encode_grpc_frame(payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).expect("gRPC message exceeds the 4 GiB frame limit");
    let mut frame = Vec::with_capacity(GRPC_FRAME_HEADER_LEN + payload.len());
    frame.push(0);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub fn grpc_frames(mut bytes: &[u8]) -> Result<Vec<&[u8]>, FramingError> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        let length = grpc_frame_length(bytes)?;
        let (frame, remaining) = bytes.split_at(length);
        frames.push(frame);
        bytes = remaining;
    }
    Ok(frames)
}

pub fn grpc_frame_length(bytes: &[u8]) -> Result<usize, FramingError> {
    if bytes.len() < GRPC_FRAME_HEADER_LEN {
        return Err(FramingError::TruncatedHeader);
    }
    let compression = *bytes.first().expect("the gRPC header length was checked");
    if compression > 1 {
        return Err(FramingError::InvalidCompressionFlag(compression));
    }
    let declared = u32::from_be_bytes(
        bytes
            .get(1..GRPC_FRAME_HEADER_LEN)
            .expect("the gRPC header length was checked")
            .try_into()
            .expect("the gRPC message length is four bytes"),
    ) as usize;
    let available = bytes.len() - GRPC_FRAME_HEADER_LEN;
    if available < declared {
        return Err(FramingError::TruncatedMessage {
            declared,
            available,
        });
    }
    Ok(GRPC_FRAME_HEADER_LEN + declared)
}

fn require_one_frame(bytes: &[u8]) -> Result<(), FramingError> {
    let count = grpc_frames(bytes)?.len();
    if count == 1 {
        Ok(())
    } else {
        Err(FramingError::ExpectedOneFrame(count))
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum FramingError {
    #[error("truncated gRPC frame header")]
    TruncatedHeader,
    #[error("invalid gRPC compression flag {0}")]
    InvalidCompressionFlag(u8),
    #[error("gRPC frame declares {declared} bytes but only {available} remain")]
    TruncatedMessage { declared: usize, available: usize },
    #[error("expected one gRPC frame, found {0}")]
    ExpectedOneFrame(usize),
    #[error("invalid fan-out response envelope: {0}")]
    InvalidEnvelope(String),
}

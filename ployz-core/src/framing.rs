use prost::{Message, Oneof};
use thiserror::Error;

use crate::{MachineId, MachineName, ValueError};

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

    #[must_use]
    pub fn omission(machine_id: &MachineId, machine_name: &MachineName) -> Self {
        Self {
            machine_id: machine_id.to_string(),
            machine_name: machine_name.to_string(),
            outcome: None,
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
        Self::decode(grpc_frame_payload(frame)?)
            .map_err(|error| FramingError::InvalidEnvelope(error.to_string()))
    }
}

pub const GRPC_FRAME_HEADER_LEN: usize = 5;

pub(crate) fn grpc_frame_payload(frame: &[u8]) -> Result<&[u8], FramingError> {
    require_one_frame(frame)?;
    Ok(frame
        .get(GRPC_FRAME_HEADER_LEN..)
        .expect("one complete gRPC frame was checked"))
}

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

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{ContainerId, MachineId, MachineName, OpaquePayload, RpcError, ServiceId, ServiceName};

const HEADER_LENGTH: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum StreamKind {
    ExecConfig = 0x01,
    ExecStdin = 0x02,
    ExecResize = 0x03,
    LogStdout = 0x10,
    LogStderr = 0x11,
    LogHeartbeat = 0x12,
    LogError = 0x13,
    ExecId = 0x81,
    ExecStdout = 0x82,
    ExecStderr = 0x83,
    ExecExit = 0x84,
    ExecError = 0x85,
}

impl TryFrom<u8> for StreamKind {
    type Error = StreamProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::ExecConfig),
            0x02 => Ok(Self::ExecStdin),
            0x03 => Ok(Self::ExecResize),
            0x10 => Ok(Self::LogStdout),
            0x11 => Ok(Self::LogStderr),
            0x12 => Ok(Self::LogHeartbeat),
            0x13 => Ok(Self::LogError),
            0x81 => Ok(Self::ExecId),
            0x82 => Ok(Self::ExecStdout),
            0x83 => Ok(Self::ExecStderr),
            0x84 => Ok(Self::ExecExit),
            0x85 => Ok(Self::ExecError),
            kind => Err(StreamProtocolError::UnknownKind(kind)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamFrame {
    kind: StreamKind,
    payload: Vec<u8>,
}

impl StreamFrame {
    fn encode(self) -> Result<OpaquePayload, StreamProtocolError> {
        let length = u32::try_from(self.payload.len())
            .map_err(|_| StreamProtocolError::PayloadTooLarge(self.payload.len()))?;
        let mut encoded = Vec::with_capacity(HEADER_LENGTH + self.payload.len());
        encoded.push(self.kind as u8);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&self.payload);
        Ok(OpaquePayload::new(encoded))
    }

    fn decode(payload: &OpaquePayload) -> Result<Self, StreamProtocolError> {
        let Some((&kind, rest)) = payload.json.split_first() else {
            return Err(StreamProtocolError::TruncatedHeader);
        };
        if rest.len() < size_of::<u32>() {
            return Err(StreamProtocolError::TruncatedHeader);
        }
        let (declared, body) = rest.split_at(size_of::<u32>());
        let declared = u32::from_be_bytes(
            declared
                .try_into()
                .expect("the length prefix was checked above"),
        ) as usize;
        if body.len() != declared {
            return Err(StreamProtocolError::LengthMismatch {
                declared,
                actual: body.len(),
            });
        }
        Ok(Self {
            kind: kind.try_into()?,
            payload: body.to_vec(),
        })
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StreamProtocolError {
    #[error("truncated stream frame header")]
    TruncatedHeader,
    #[error("unknown stream frame kind {0:#04x}")]
    UnknownKind(u8),
    #[error("stream frame declares {declared} payload bytes but carries {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("stream frame payload is too large: {0} bytes")]
    PayloadTooLarge(usize),
    #[error("expected {expected} stream frame, received {actual:?}")]
    UnexpectedKind {
        expected: &'static str,
        actual: String,
    },
    #[error("invalid {kind} stream frame payload: {message}")]
    InvalidPayload { kind: &'static str, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
// TODO(EO-005): exec user, privilege, working-directory, and environment options
// remain outside the reconstructed contract.
pub struct ExecOptions {
    pub command: Vec<String>,
    pub attach_stdin: bool,
    pub attach_stdout: bool,
    pub attach_stderr: bool,
    pub tty: bool,
    pub detach: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecConfig {
    pub container_id: ContainerId,
    pub options: ExecOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecRequestFrame {
    Config(ExecConfig),
    Stdin(Vec<u8>),
    Resize { width: u16, height: u16 },
}

impl ExecRequestFrame {
    pub fn encode(&self) -> Result<OpaquePayload, StreamProtocolError> {
        let (kind, payload) = match self {
            Self::Config(config) => (StreamKind::ExecConfig, json(config, "exec config")?),
            Self::Stdin(bytes) => (StreamKind::ExecStdin, bytes.clone()),
            Self::Resize { width, height } => {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&width.to_be_bytes());
                payload.extend_from_slice(&height.to_be_bytes());
                (StreamKind::ExecResize, payload)
            }
        };
        StreamFrame { kind, payload }.encode()
    }

    pub fn decode(payload: &OpaquePayload) -> Result<Self, StreamProtocolError> {
        let frame = StreamFrame::decode(payload)?;
        match frame.kind {
            StreamKind::ExecConfig => Ok(Self::Config(decode_json(&frame.payload, "exec config")?)),
            StreamKind::ExecStdin => Ok(Self::Stdin(frame.payload)),
            StreamKind::ExecResize => {
                let bytes: [u8; 4] = frame
                    .payload
                    .try_into()
                    .map_err(|_| invalid("exec resize", "expected two u16 dimensions"))?;
                let [width_high, width_low, height_high, height_low] = bytes;
                Ok(Self::Resize {
                    width: u16::from_be_bytes([width_high, width_low]),
                    height: u16::from_be_bytes([height_high, height_low]),
                })
            }
            actual @ (StreamKind::LogStdout
            | StreamKind::LogStderr
            | StreamKind::LogHeartbeat
            | StreamKind::LogError
            | StreamKind::ExecId
            | StreamKind::ExecStdout
            | StreamKind::ExecStderr
            | StreamKind::ExecExit
            | StreamKind::ExecError) => Err(unexpected("exec request", actual)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecResponseFrame {
    ExecId(String),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
    Error(RpcError),
}

impl ExecResponseFrame {
    pub fn encode(&self) -> Result<OpaquePayload, StreamProtocolError> {
        let (kind, payload) = match self {
            Self::ExecId(id) => (StreamKind::ExecId, id.as_bytes().to_vec()),
            Self::Stdout(bytes) => (StreamKind::ExecStdout, bytes.clone()),
            Self::Stderr(bytes) => (StreamKind::ExecStderr, bytes.clone()),
            Self::Exit(code) => (StreamKind::ExecExit, code.to_be_bytes().to_vec()),
            Self::Error(error) => (StreamKind::ExecError, json(error, "exec error")?),
        };
        StreamFrame { kind, payload }.encode()
    }

    pub fn decode(payload: &OpaquePayload) -> Result<Self, StreamProtocolError> {
        let frame = StreamFrame::decode(payload)?;
        match frame.kind {
            StreamKind::ExecId => String::from_utf8(frame.payload)
                .map(Self::ExecId)
                .map_err(|error| invalid("exec ID", error.to_string())),
            StreamKind::ExecStdout => Ok(Self::Stdout(frame.payload)),
            StreamKind::ExecStderr => Ok(Self::Stderr(frame.payload)),
            StreamKind::ExecExit => {
                let bytes: [u8; 4] = frame
                    .payload
                    .try_into()
                    .map_err(|_| invalid("exec exit", "expected one i32"))?;
                Ok(Self::Exit(i32::from_be_bytes(bytes)))
            }
            StreamKind::ExecError => Ok(Self::Error(decode_json(&frame.payload, "exec error")?)),
            actual @ (StreamKind::ExecConfig
            | StreamKind::ExecStdin
            | StreamKind::ExecResize
            | StreamKind::LogStdout
            | StreamKind::LogStderr
            | StreamKind::LogHeartbeat
            | StreamKind::LogError) => Err(unexpected("exec response", actual)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MachineLogService {
    Ployz,
    Docker,
    Corrosion,
}

impl MachineLogService {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ployz => "ployz",
            Self::Docker => "docker",
            Self::Corrosion => "corrosion",
        }
    }
}

impl fmt::Display for MachineLogService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MachineLogService {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ployz" => Ok(Self::Ployz),
            "docker" => Ok(Self::Docker),
            "corrosion" => Ok(Self::Corrosion),
            _ => Err("expected ployz, docker, or corrosion"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "origin")]
pub enum LogOrigin {
    Service {
        service_id: ServiceId,
        service_name: ServiceName,
        container_id: ContainerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook: Option<String>,
    },
    Machine {
        service: MachineLogService,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogMetadata {
    pub origin: LogOrigin,
    pub machine_id: MachineId,
    pub machine_name: MachineName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogStream {
    Stdout,
    Stderr,
    Heartbeat,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub metadata: LogMetadata,
    pub stream: LogStream,
    pub timestamp_unix_nanos: i64,
    pub message: Vec<u8>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct LogHeader {
    metadata: LogMetadata,
    timestamp_unix_nanos: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl LogEntry {
    #[must_use]
    pub fn error(metadata: LogMetadata, message: impl Into<String>) -> Self {
        Self {
            metadata,
            stream: LogStream::Error,
            timestamp_unix_nanos: 0,
            message: Vec::new(),
            error: Some(message.into()),
        }
    }

    #[must_use]
    pub fn heartbeat(metadata: LogMetadata, timestamp_unix_nanos: i64) -> Self {
        Self {
            metadata,
            stream: LogStream::Heartbeat,
            timestamp_unix_nanos,
            message: Vec::new(),
            error: None,
        }
    }

    pub fn encode(&self) -> Result<OpaquePayload, StreamProtocolError> {
        let kind = match self.stream {
            LogStream::Stdout => StreamKind::LogStdout,
            LogStream::Stderr => StreamKind::LogStderr,
            LogStream::Heartbeat => StreamKind::LogHeartbeat,
            LogStream::Error => StreamKind::LogError,
        };
        let header = json(
            &LogHeader {
                metadata: self.metadata.clone(),
                timestamp_unix_nanos: self.timestamp_unix_nanos,
                error: self.error.clone(),
            },
            "log header",
        )?;
        let header_length = u32::try_from(header.len())
            .map_err(|_| StreamProtocolError::PayloadTooLarge(header.len()))?;
        let mut payload = Vec::with_capacity(4 + header.len() + self.message.len());
        payload.extend_from_slice(&header_length.to_be_bytes());
        payload.extend_from_slice(&header);
        payload.extend_from_slice(&self.message);
        StreamFrame { kind, payload }.encode()
    }

    pub fn decode(payload: &OpaquePayload) -> Result<Self, StreamProtocolError> {
        let frame = StreamFrame::decode(payload)?;
        let stream = match frame.kind {
            StreamKind::LogStdout => LogStream::Stdout,
            StreamKind::LogStderr => LogStream::Stderr,
            StreamKind::LogHeartbeat => LogStream::Heartbeat,
            StreamKind::LogError => LogStream::Error,
            actual @ (StreamKind::ExecConfig
            | StreamKind::ExecStdin
            | StreamKind::ExecResize
            | StreamKind::ExecId
            | StreamKind::ExecStdout
            | StreamKind::ExecStderr
            | StreamKind::ExecExit
            | StreamKind::ExecError) => return Err(unexpected("log entry", actual)),
        };
        if frame.payload.len() < size_of::<u32>() {
            return Err(invalid("log entry", "truncated metadata length"));
        }
        let (header_length, rest) = frame.payload.split_at(size_of::<u32>());
        let header_length = u32::from_be_bytes(
            header_length
                .try_into()
                .expect("the metadata length prefix was checked above"),
        ) as usize;
        if rest.len() < header_length {
            return Err(invalid("log entry", "truncated metadata"));
        }
        let (header, message) = rest.split_at(header_length);
        let header: LogHeader = decode_json(header, "log header")?;
        Ok(Self {
            metadata: header.metadata,
            stream,
            timestamp_unix_nanos: header.timestamp_unix_nanos,
            message: message.to_vec(),
            error: header.error,
        })
    }
}

fn json(value: &impl Serialize, kind: &'static str) -> Result<Vec<u8>, StreamProtocolError> {
    serde_json::to_vec(value).map_err(|error| invalid(kind, error.to_string()))
}

fn decode_json<T: DeserializeOwned>(
    payload: &[u8],
    kind: &'static str,
) -> Result<T, StreamProtocolError> {
    serde_json::from_slice(payload).map_err(|error| invalid(kind, error.to_string()))
}

fn unexpected(expected: &'static str, actual: StreamKind) -> StreamProtocolError {
    StreamProtocolError::UnexpectedKind {
        expected,
        actual: format!("{actual:?}"),
    }
}

fn invalid(kind: &'static str, message: impl Into<String>) -> StreamProtocolError {
    StreamProtocolError::InvalidPayload {
        kind,
        message: message.into(),
    }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ProtocolError, ProtocolResult};
use crate::operation::{OperationEventDto, OperationIdDto};
use crate::status::StatusResponseDto;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestId(String);

impl RequestId {
    pub fn parse(value: impl Into<String>) -> ProtocolResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ProtocolError::malformed_payload(
                "request id must not be empty",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestFrame {
    id: RequestId,
    method: String,
    #[serde(default)]
    payload: Value,
}

impl RequestFrame {
    #[must_use]
    pub fn new(id: RequestId, method: impl Into<String>, payload: Value) -> Self {
        Self {
            id,
            method: method.into(),
            payload,
        }
    }

    #[must_use]
    pub fn id(&self) -> &RequestId {
        &self.id
    }

    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    #[must_use]
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    pub fn decode(line: &str) -> ProtocolResult<Self> {
        let raw: RawRequestFrame = serde_json::from_str(line)
            .map_err(|source| ProtocolError::parse(source.to_string()))?;
        let id = RequestId::parse(raw.id)?;
        if raw.method.trim().is_empty() {
            return Err(ProtocolError::malformed_payload(
                "request method must not be empty",
            ));
        }
        Ok(Self {
            id,
            method: raw.method,
            payload: raw.payload.unwrap_or(Value::Null),
        })
    }

    pub fn decode_payload<T>(&self) -> ProtocolResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        serde_json::from_value(self.payload.clone()).map_err(|source| {
            ProtocolError::malformed_payload(source.to_string()).with_method(self.method.clone())
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawRequestFrame {
    id: String,
    method: String,
    payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseFrame {
    id: RequestId,
    result: ResponsePayload,
}

impl ResponseFrame {
    #[must_use]
    pub fn new(id: RequestId, result: ResponsePayload) -> Self {
        Self { id, result }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorFrame {
    id: Option<RequestId>,
    error: ProtocolError,
}

impl ErrorFrame {
    #[must_use]
    pub fn new(id: Option<RequestId>, error: ProtocolError) -> Self {
        Self { id, error }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFrame {
    Response(ResponseFrame),
    Error(ErrorFrame),
}

impl OutputFrame {
    #[must_use]
    pub fn response(id: RequestId, result: ResponsePayload) -> Self {
        Self::Response(ResponseFrame::new(id, result))
    }

    #[must_use]
    pub fn error(id: Option<RequestId>, error: ProtocolError) -> Self {
        Self::Error(ErrorFrame::new(id, error))
    }

    pub fn encode(&self) -> ProtocolResult<String> {
        serde_json::to_string(self)
            .map_err(|source| ProtocolError::malformed_payload(source.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponsePayload {
    Status(StatusResponseDto),
    OperationSubmitted { operation: OperationIdDto },
    OperationStatus(crate::operation::OperationStatusResponseDto),
    OperationEvents { events: Vec<OperationEventDto> },
}

//! One-shot Build messages carried by the authenticated Ployz stream.
//! No BuildKit endpoint, host paths, or execution policy crosses this boundary.

use crate::{BuiltImage, Output, Progress, Stage, Target};
use ployz_core::{MachineId, OpaquePayload};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::received_recipe::{validate_capture, validate_remote_context};
pub use crate::upload::{Upload, upload};

/// Invalid or unavailable captured input at the transport trust boundary.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct InputError(String);
impl From<String> for InputError {
    fn from(message: String) -> Self {
        Self(message)
    }
}
impl From<&str> for InputError {
    fn from(message: &str) -> Self {
        Self(message.into())
    }
}

/// Largest JSON envelope accepted by the Build protocol.
pub const FRAME_LIMIT: usize = 256 * 1024;
/// Source bytes per bounded upload frame.
pub const CHUNK_SIZE: usize = 32 * 1024;

/// Captured recipe targets and explicit execution options.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Targets in this admitted attempt.
    pub targets: Vec<Target>,
    /// Requested disposition of completed output.
    pub output: Output,
    /// Disable cache reuse for this attempt.
    pub no_cache: bool,
    /// Request upstream base-image refresh.
    pub pull: bool,
}

/// Captured filesystem entry kind; special files are unsupported.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Kind {
    Directory,
    File { size: u64 },
    Link { target: Vec<u8> },
}

/// Ordered client messages for one connection-scoped attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Input {
    Start(Definition),
    Entry {
        path: Vec<u8>,
        kind: Kind,
        mode: u32,
    },
    Data(Vec<u8>),
    Finish,
    Cancel,
}

/// Admission, observed progress, or the terminal host report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Admitted { machine_id: MachineId },
    Progress(Progress),
    Finished(Outcome),
}

/// Terminal evidence, including known work when execution fails.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Outcome {
    Images {
        machine_id: MachineId,
        images: Vec<BuiltImage>,
    },
    Validated {
        machine_id: MachineId,
    },
    Published {
        machine_id: MachineId,
    },
    Failed {
        stage: Stage,
        message: String,
        work: crate::WorkEvidence,
    },
    Unknown {
        stage: Stage,
        message: String,
        work: crate::WorkEvidence,
    },
}

/// Enforce the message limit on both sides, before deserialization allocates
/// strings or entry lists. Tonic also bounds each outer protobuf envelope.
/// # Errors
/// Rejects oversized or malformed frames.
pub fn decode<T: DeserializeOwned>(payload: &OpaquePayload) -> Result<T, InputError> {
    if payload.json.len() > FRAME_LIMIT {
        return Err("Build frame exceeds its size limit".into());
    }
    serde_json::from_slice(&payload.json).map_err(|_| "invalid Build frame".into())
}

/// Serialize one bounded Build message.
/// # Errors
/// Rejects serialization failures and oversized frames.
pub fn encode<T: Serialize>(value: &T) -> Result<OpaquePayload, InputError> {
    let bytes = serde_json::to_vec(value).map_err(|_| "cannot encode Build frame")?;
    if bytes.len() > FRAME_LIMIT {
        return Err("Build frame exceeds its size limit".into());
    }
    Ok(OpaquePayload::new(bytes))
}

impl Outcome {
    /// Attach observed work without changing the terminal diagnosis.
    #[must_use]
    pub fn with_work(mut self, evidence: crate::WorkEvidence) -> Self {
        match &mut self {
            Self::Failed { work, .. } | Self::Unknown { work, .. } => *work = evidence,
            Self::Images { .. } | Self::Validated { .. } | Self::Published { .. } => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_messages_are_rejected_before_sending() {
        assert!(super::encode(&"x".repeat(super::FRAME_LIMIT)).is_err());
    }
}

use serde::{Deserialize, Serialize};

use crate::ProtocolErrorCode;

pub const METHOD_STATUS: &str = "status";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponseDto {
    command_service_ready: bool,
    daemon_ready: bool,
    endpoint_id: Option<String>,
    startup: StartupReportDto,
    last_operation_failure: Option<OperationFailureSummaryDto>,
}

impl StatusResponseDto {
    #[must_use]
    pub fn new(
        command_service_ready: bool,
        daemon_ready: bool,
        endpoint_id: Option<String>,
        startup: StartupReportDto,
        last_operation_failure: Option<OperationFailureSummaryDto>,
    ) -> Self {
        Self {
            command_service_ready,
            daemon_ready,
            endpoint_id,
            startup,
            last_operation_failure,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupReportDto {
    corrosion_phase: CorrosionPhaseDto,
    corrosion_cleanup: CorrosionCleanupDto,
    schema_phase: SchemaPhaseDto,
    peer_phase: PeerPhaseDto,
    corrosion_api_addr: String,
    peer_identity_path: String,
}

impl StartupReportDto {
    #[must_use]
    pub fn new(
        corrosion_phase: CorrosionPhaseDto,
        corrosion_cleanup: CorrosionCleanupDto,
        schema_phase: SchemaPhaseDto,
        peer_phase: PeerPhaseDto,
        corrosion_api_addr: impl Into<String>,
        peer_identity_path: impl Into<String>,
    ) -> Self {
        Self {
            corrosion_phase,
            corrosion_cleanup,
            schema_phase,
            peer_phase,
            corrosion_api_addr: corrosion_api_addr.into(),
            peer_identity_path: peer_identity_path.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionPhaseDto {
    Configured,
    Ready,
    Failed { failure: CorrosionFailureDto },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrosionFailureDto {
    Agent,
    Store,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionCleanupDto {
    NotRun,
    Stopped { stop: CorrosionStopDto },
    Unmanaged,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrosionStopDto {
    Graceful,
    Forced,
    UnexpectedExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchemaPhaseDto {
    Configured,
    Ready,
    Failed { failure: SchemaFailureDto },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFailureDto {
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerPhaseDto {
    Configured,
    Ready,
    Failed { failure: PeerFailureDto },
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerFailureDto {
    Startup,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationFailureSummaryDto {
    operation: String,
    code: ProtocolErrorCode,
}

impl OperationFailureSummaryDto {
    #[must_use]
    pub fn new(operation: impl Into<String>, code: ProtocolErrorCode) -> Self {
        Self {
            operation: operation.into(),
            code,
        }
    }
}

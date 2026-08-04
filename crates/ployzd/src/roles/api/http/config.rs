//! Environment-backed configuration for the API role.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use ployz_core::ids::{ClusterId, MachineRowId};

use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};

const CORROSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CORROSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CORROSION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const CORROSION_MAX_NDJSON_FRAME_BYTES: usize = 1_048_576;
const CORROSION_MAX_ERROR_BODY_BYTES: usize = 65_536;
const MAX_BUILD_DIAGNOSTIC_BYTES: usize = 512;

const CORROSION_API_ADDR_ENV: &str = "PLOYZ_CORROSION_API_ADDR";
const CORROSION_BEARER_TOKEN_ENV: &str = "PLOYZ_CORROSION_BEARER_TOKEN";
const CLUSTER_ID_ENV: &str = "PLOYZ_CLUSTER_ID";
const MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const API_LISTEN_ADDR_ENV: &str = "PLOYZ_API_LISTEN_ADDR";
const BUILD_ENV: &str = "PLOYZ_BUILD";

/// Configuration supplied by the local `ployzd` role environment file.
///
/// The listener still undergoes roster-backed validation at startup: a
/// syntactically concrete address is not enough to prove it is this machine's
/// mesh address.
#[derive(Debug, Clone)]
pub struct ApiRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    listen_addr: SocketAddr,
    build: String,
}

impl ApiRoleConfig {
    /// Parses the API role's local environment-file inputs.
    pub fn from_environment() -> Result<Self, ApiRoleConfigError> {
        let corrosion_api_addr = environment_value(CORROSION_API_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| ApiRoleConfigError::InvalidSocketAddress {
                name: CORROSION_API_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let bearer_token = BearerToken::new(environment_value(CORROSION_BEARER_TOKEN_ENV)?)
            .map_err(ApiRoleConfigError::CorrosionConfig)?;
        let corrosion = CorrosionClientConfig::new(
            corrosion_api_addr,
            bearer_token,
            CorrosionClientBounds {
                connect_timeout: CORROSION_CONNECT_TIMEOUT,
                request_timeout: CORROSION_REQUEST_TIMEOUT,
                stream_idle_timeout: CORROSION_STREAM_IDLE_TIMEOUT,
                max_ndjson_frame_bytes: CORROSION_MAX_NDJSON_FRAME_BYTES,
                max_error_body_bytes: CORROSION_MAX_ERROR_BODY_BYTES,
            },
        )
        .map_err(ApiRoleConfigError::CorrosionConfig)?;
        let cluster_id =
            ClusterId::try_new(environment_value(CLUSTER_ID_ENV)?).map_err(|error| {
                ApiRoleConfigError::InvalidClusterId {
                    detail: error.to_string(),
                }
            })?;
        let local_machine_id =
            MachineRowId::try_new(environment_value(MACHINE_ID_ENV)?).map_err(|error| {
                ApiRoleConfigError::InvalidMachineId {
                    detail: error.to_string(),
                }
            })?;
        let listen_addr = environment_value(API_LISTEN_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| ApiRoleConfigError::InvalidSocketAddress {
                name: API_LISTEN_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let build = validate_build_diagnostic(environment_value(BUILD_ENV)?)?;

        Self::new(corrosion, cluster_id, local_machine_id, listen_addr, build)
    }

    /// Builds validated production configuration without reading the process
    /// environment. Tests and process wiring use this to keep credentials out
    /// of command-line arguments.
    pub fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
    ) -> Result<Self, ApiRoleConfigError> {
        if listen_addr.ip().is_unspecified() {
            return Err(ApiRoleConfigError::WildcardListenAddress { listen_addr });
        }
        if listen_addr.port() == 0 {
            return Err(ApiRoleConfigError::ZeroListenPort);
        }
        let build = validate_build_diagnostic(build)?;

        Ok(Self {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
        })
    }

    #[must_use]
    pub fn corrosion(&self) -> &CorrosionClientConfig {
        &self.corrosion
    }

    #[must_use]
    pub fn cluster_id(&self) -> &ClusterId {
        &self.cluster_id
    }

    #[must_use]
    pub fn local_machine_id(&self) -> &MachineRowId {
        &self.local_machine_id
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub fn build(&self) -> &str {
        &self.build
    }
}

fn environment_value(name: &'static str) -> Result<String, ApiRoleConfigError> {
    env::var(name).map_err(|error| match error {
        env::VarError::NotPresent => ApiRoleConfigError::MissingEnvironment { name },
        env::VarError::NotUnicode(_) => ApiRoleConfigError::NonUnicodeEnvironment { name },
    })
}

fn validate_build_diagnostic(value: String) -> Result<String, ApiRoleConfigError> {
    if value.trim().is_empty() {
        return Err(ApiRoleConfigError::EmptyBuildDiagnostic);
    }
    if value.len() > MAX_BUILD_DIAGNOSTIC_BYTES {
        return Err(ApiRoleConfigError::BuildDiagnosticTooLong {
            limit: MAX_BUILD_DIAGNOSTIC_BYTES,
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ApiRoleConfigError::InvalidBuildDiagnostic);
    }
    Ok(value)
}

/// A local API-role startup configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiRoleConfigError {
    #[error("required API role environment variable {name} is missing")]
    MissingEnvironment { name: &'static str },
    #[error("API role environment variable {name} is not valid Unicode")]
    NonUnicodeEnvironment { name: &'static str },
    #[error("API role environment variable {name} is not a socket address: {detail}")]
    InvalidSocketAddress { name: &'static str, detail: String },
    #[error("API listener address {listen_addr} must not be wildcard")]
    WildcardListenAddress { listen_addr: SocketAddr },
    #[error("API listener port must be nonzero")]
    ZeroListenPort,
    #[error("PLOYZ_CLUSTER_ID is invalid: {detail}")]
    InvalidClusterId { detail: String },
    #[error("PLOYZ_MACHINE_ID is invalid: {detail}")]
    InvalidMachineId { detail: String },
    #[error("PLOYZ_BUILD must not be empty")]
    EmptyBuildDiagnostic,
    #[error("PLOYZ_BUILD exceeds the {limit}-byte diagnostic limit")]
    BuildDiagnosticTooLong { limit: usize },
    #[error("PLOYZ_BUILD must not contain control characters")]
    InvalidBuildDiagnostic,
    #[error("invalid local Corrosion configuration: {0}")]
    CorrosionConfig(#[source] crate::corrosion::CorrosionClientConfigError),
}

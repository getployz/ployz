//! Environment-backed configuration for the API role.

use std::env;
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use hmac::{Hmac, Mac};
use ployz_core::ids::{ClusterId, MachineRowId};
use sha2::Sha256;

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
const BOOTSTRAP_SECRET_ENV: &str = "PLOYZ_API_BOOTSTRAP_SECRET";
const MAX_BOOTSTRAP_SECRET_BYTES: usize = 4_096;
const BOOTSTRAP_SECRET_DOMAIN: &[u8] = b"ployz-api-founding-v1";

type BootstrapMac = Hmac<Sha256>;

/// The API startup authority mode.
#[derive(Clone)]
pub enum ApiRoleMode {
    /// The listener and every caller are authenticated from converged roster rows.
    Ordinary,
    /// The one local founding endpoint is available before roster rows exist.
    Founding(BootstrapSecret),
}

impl fmt::Debug for ApiRoleMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ordinary => formatter.write_str("Ordinary"),
            Self::Founding(_) => formatter.write_str("Founding([REDACTED])"),
        }
    }
}

/// A bootstrap credential retained only as a one-way authentication tag.
#[derive(Clone)]
pub struct BootstrapSecret([u8; 32]);

impl BootstrapSecret {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, ApiRoleConfigError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(ApiRoleConfigError::EmptyBootstrapSecret);
        }
        if value.len() > MAX_BOOTSTRAP_SECRET_BYTES {
            return Err(ApiRoleConfigError::BootstrapSecretTooLong {
                limit: MAX_BOOTSTRAP_SECRET_BYTES,
            });
        }
        Ok(Self(bootstrap_tag(value)))
    }

    #[must_use]
    pub fn verifies(&self, candidate: &[u8]) -> bool {
        let Ok(mut verifier) = BootstrapMac::new_from_slice(candidate) else {
            return false;
        };
        verifier.update(BOOTSTRAP_SECRET_DOMAIN);
        verifier.verify_slice(&self.0).is_ok()
    }
}

impl fmt::Debug for BootstrapSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BootstrapSecret([REDACTED])")
    }
}

fn bootstrap_tag(value: &[u8]) -> [u8; 32] {
    let mut mac = BootstrapMac::new_from_slice(value)
        .expect("HMAC accepts bootstrap credentials of every byte length");
    mac.update(BOOTSTRAP_SECRET_DOMAIN);
    mac.finalize().into_bytes().into()
}

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
    mode: ApiRoleMode,
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

        let mode = match env::var(BOOTSTRAP_SECRET_ENV) {
            Ok(value) => ApiRoleMode::Founding(BootstrapSecret::new(value.as_bytes())?),
            Err(env::VarError::NotPresent) => ApiRoleMode::Ordinary,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(ApiRoleConfigError::NonUnicodeEnvironment {
                    name: BOOTSTRAP_SECRET_ENV,
                });
            }
        };

        Self::new_with_mode(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            mode,
        )
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
        Self::new_with_mode(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            ApiRoleMode::Ordinary,
        )
    }

    /// Builds founding-mode configuration around a local bootstrap credential.
    pub fn new_founding(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
        bootstrap_secret: BootstrapSecret,
    ) -> Result<Self, ApiRoleConfigError> {
        Self::new_with_mode(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            ApiRoleMode::Founding(bootstrap_secret),
        )
    }

    fn new_with_mode(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
        mode: ApiRoleMode,
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
            mode,
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

    #[must_use]
    pub const fn mode(&self) -> &ApiRoleMode {
        &self.mode
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
    #[error("PLOYZ_API_BOOTSTRAP_SECRET must not be empty")]
    EmptyBootstrapSecret,
    #[error("PLOYZ_API_BOOTSTRAP_SECRET exceeds the {limit}-byte limit")]
    BootstrapSecretTooLong { limit: usize },
    #[error("invalid local Corrosion configuration: {0}")]
    CorrosionConfig(#[source] crate::corrosion::CorrosionClientConfigError),
}

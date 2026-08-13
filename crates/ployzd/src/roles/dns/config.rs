//! Environment-backed configuration for the DNS role.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use ployz_core::ids::{ClusterName, MachineName};

use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};

const CORROSION_API_ADDR_ENV: &str = "PLOYZ_CORROSION_API_ADDR";
const CORROSION_BEARER_TOKEN_ENV: &str = "PLOYZ_CORROSION_BEARER_TOKEN";
const CLUSTER_ID_ENV: &str = "PLOYZ_CLUSTER_ID";
const MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";

const CORROSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CORROSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CORROSION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const CORROSION_MAX_NDJSON_FRAME_BYTES: usize = 1_048_576;
const CORROSION_MAX_ERROR_BODY_BYTES: usize = 65_536;

#[derive(Debug, Clone)]
pub struct DnsRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
}

impl DnsRoleConfig {
    pub fn from_environment() -> Result<Self, DnsRoleConfigError> {
        let corrosion_api_addr = required_environment(CORROSION_API_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| DnsRoleConfigError::InvalidSocketAddress {
                name: CORROSION_API_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let bearer_token = BearerToken::new(required_environment(CORROSION_BEARER_TOKEN_ENV)?)
            .map_err(DnsRoleConfigError::CorrosionConfiguration)?;
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
        .map_err(DnsRoleConfigError::CorrosionConfiguration)?;
        let cluster_id =
            ClusterName::try_new(required_environment(CLUSTER_ID_ENV)?).map_err(|error| {
                DnsRoleConfigError::InvalidClusterId {
                    detail: error.to_string(),
                }
            })?;
        let local_machine_id = MachineName::try_new(required_environment(MACHINE_ID_ENV)?)
            .map_err(|error| DnsRoleConfigError::InvalidMachineId {
                detail: error.to_string(),
            })?;
        Ok(Self::new(corrosion, cluster_id, local_machine_id))
    }

    #[must_use]
    pub const fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
    ) -> Self {
        Self {
            corrosion,
            cluster_id,
            local_machine_id,
        }
    }

    #[must_use]
    pub const fn corrosion(&self) -> &CorrosionClientConfig {
        &self.corrosion
    }

    #[must_use]
    pub const fn cluster_id(&self) -> &ClusterName {
        &self.cluster_id
    }

    #[must_use]
    pub const fn local_machine_id(&self) -> &MachineName {
        &self.local_machine_id
    }
}

fn required_environment(name: &'static str) -> Result<String, DnsRoleConfigError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => Err(DnsRoleConfigError::MissingEnvironment(name)),
        Err(env::VarError::NotUnicode(_)) => Err(DnsRoleConfigError::NonUnicodeEnvironment(name)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DnsRoleConfigError {
    #[error("required environment variable {0} is missing or empty")]
    MissingEnvironment(&'static str),
    #[error("environment variable {0} is not Unicode")]
    NonUnicodeEnvironment(&'static str),
    #[error("{name} is not a socket address: {detail}")]
    InvalidSocketAddress { name: &'static str, detail: String },
    #[error("PLOYZ_CLUSTER_ID is invalid: {detail}")]
    InvalidClusterId { detail: String },
    #[error("PLOYZ_MACHINE_ID is invalid: {detail}")]
    InvalidMachineId { detail: String },
    #[error(transparent)]
    CorrosionConfiguration(crate::corrosion::CorrosionClientConfigError),
}

//! Environment-backed configuration for the public gateway role.

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use ployz_core::ids::{ClusterName, MachineName};

use crate::corrosion::{BearerToken, CorrosionClientBounds, CorrosionClientConfig};

const CORROSION_API_ADDR_ENV: &str = "PLOYZ_CORROSION_API_ADDR";
const CORROSION_BEARER_TOKEN_ENV: &str = "PLOYZ_CORROSION_BEARER_TOKEN";
const CLUSTER_ID_ENV: &str = "PLOYZ_CLUSTER_ID";
const MACHINE_ID_ENV: &str = "PLOYZ_MACHINE_ID";
const GATEWAY_LISTEN_ADDR_ENV: &str = "PLOYZ_GATEWAY_LISTEN_ADDR";
const DEFAULT_GATEWAY_LISTEN_ADDR: &str = "0.0.0.0:80";

const CORROSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const CORROSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const CORROSION_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const CORROSION_MAX_NDJSON_FRAME_BYTES: usize = 1_048_576;
const CORROSION_MAX_ERROR_BODY_BYTES: usize = 65_536;

#[derive(Debug, Clone)]
pub struct GatewayRoleConfig {
    corrosion: CorrosionClientConfig,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
    listen_addr: SocketAddr,
}

impl GatewayRoleConfig {
    pub fn from_environment() -> Result<Self, GatewayRoleConfigError> {
        let corrosion_api_addr = required_environment(CORROSION_API_ADDR_ENV)?
            .parse::<SocketAddr>()
            .map_err(|error| GatewayRoleConfigError::InvalidSocketAddress {
                name: CORROSION_API_ADDR_ENV,
                detail: error.to_string(),
            })?;
        let bearer_token = BearerToken::new(required_environment(CORROSION_BEARER_TOKEN_ENV)?)
            .map_err(GatewayRoleConfigError::CorrosionConfiguration)?;
        let corrosion =
            CorrosionClientConfig::new(corrosion_api_addr, bearer_token, corrosion_bounds())
                .map_err(GatewayRoleConfigError::CorrosionConfiguration)?;
        let cluster_id =
            ClusterName::try_new(required_environment(CLUSTER_ID_ENV)?).map_err(|error| {
                GatewayRoleConfigError::InvalidClusterId {
                    detail: error.to_string(),
                }
            })?;
        let local_machine_id = MachineName::try_new(required_environment(MACHINE_ID_ENV)?)
            .map_err(|error| GatewayRoleConfigError::InvalidMachineId {
                detail: error.to_string(),
            })?;
        let listen_addr = optional_environment(GATEWAY_LISTEN_ADDR_ENV)?
            .unwrap_or_else(|| DEFAULT_GATEWAY_LISTEN_ADDR.to_owned())
            .parse::<SocketAddr>()
            .map_err(|error| GatewayRoleConfigError::InvalidSocketAddress {
                name: GATEWAY_LISTEN_ADDR_ENV,
                detail: error.to_string(),
            })?;
        Ok(Self::new(
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
        ))
    }

    #[must_use]
    pub const fn new(
        corrosion: CorrosionClientConfig,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
        listen_addr: SocketAddr,
    ) -> Self {
        Self {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
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

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

const fn corrosion_bounds() -> CorrosionClientBounds {
    CorrosionClientBounds {
        connect_timeout: CORROSION_CONNECT_TIMEOUT,
        request_timeout: CORROSION_REQUEST_TIMEOUT,
        stream_idle_timeout: CORROSION_STREAM_IDLE_TIMEOUT,
        max_ndjson_frame_bytes: CORROSION_MAX_NDJSON_FRAME_BYTES,
        max_error_body_bytes: CORROSION_MAX_ERROR_BODY_BYTES,
    }
}

fn required_environment(name: &'static str) -> Result<String, GatewayRoleConfigError> {
    optional_environment(name)?.ok_or(GatewayRoleConfigError::MissingEnvironment(name))
}

fn optional_environment(name: &'static str) -> Result<Option<String>, GatewayRoleConfigError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        Ok(_) | Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(GatewayRoleConfigError::NonUnicodeEnvironment(name))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayRoleConfigError {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_defaults_and_corrosion_bounds_are_the_public_role_contract() {
        assert_eq!(
            DEFAULT_GATEWAY_LISTEN_ADDR
                .parse::<SocketAddr>()
                .expect("default"),
            SocketAddr::from(([0, 0, 0, 0], 80))
        );
        assert_eq!(GATEWAY_LISTEN_ADDR_ENV, "PLOYZ_GATEWAY_LISTEN_ADDR");
        assert_eq!(MACHINE_ID_ENV, "PLOYZ_MACHINE_ID");
        assert_eq!(
            corrosion_bounds(),
            CorrosionClientBounds {
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(2),
                stream_idle_timeout: Duration::from_secs(45),
                max_ndjson_frame_bytes: 1_048_576,
                max_error_body_bytes: 65_536,
            }
        );
    }
}

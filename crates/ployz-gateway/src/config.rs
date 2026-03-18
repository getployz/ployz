use std::path::PathBuf;

use thiserror::Error;

pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:80";
pub const DEFAULT_THREADS: usize = 2;

// ---------------------------------------------------------------------------
// GatewayError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("gateway config error: {0}")]
    Config(String),
    #[error("failed to reach routing store: {0}")]
    Store(String),
    #[error("projection failed: {0}")]
    Projection(String),
    #[error("gateway runtime failed: {0}")]
    Runtime(String),
    #[error("gateway process failed: {0}")]
    Process(String),
}

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub data_dir: PathBuf,
    pub network: String,
    pub listen_addr: String,
    pub threads: usize,
}

impl GatewayConfig {
    #[must_use]
    pub fn for_network(
        data_dir: &std::path::Path,
        network: &str,
        listen_addr: String,
        threads: usize,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            network: network.to_string(),
            listen_addr,
            threads,
        }
    }

    pub fn from_env() -> Result<Self, GatewayError> {
        let data_dir = match std::env::var_os("PLOYZ_GATEWAY_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => {
                let aff = ployz_config::Affordances::detect();
                ployz_config::default_data_dir(&aff)
            }
        };
        let network = match std::env::var("PLOYZ_GATEWAY_NETWORK") {
            Ok(network) if !network.trim().is_empty() => network,
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_NETWORK was set but empty".into(),
                ));
            }
            Err(_) => ployz_types::paths::read_active_network(&data_dir)
                .ok_or_else(|| GatewayError::Config("no active network marker was found".into()))?,
        };
        let listen_addr = match std::env::var("PLOYZ_GATEWAY_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => address,
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => DEFAULT_LISTEN_ADDR.to_string(),
        };
        let threads = match std::env::var("PLOYZ_GATEWAY_THREADS") {
            Ok(raw) => raw.parse::<usize>().map_err(|err| {
                GatewayError::Config(format!(
                    "invalid PLOYZ_GATEWAY_THREADS value '{raw}': {err}"
                ))
            })?,
            Err(_) => DEFAULT_THREADS,
        };

        Ok(Self {
            data_dir,
            network,
            listen_addr,
            threads,
        })
    }
}

use std::path::{Path, PathBuf};

use thiserror::Error;

use ployz_sdk::model::OverlayIp;

// ---------------------------------------------------------------------------
// DnsError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum DnsError {
    #[error("dns config error: {0}")]
    Config(String),
    #[error("failed to reach routing store: {0}")]
    Store(String),
    #[error("dns runtime failed: {0}")]
    Runtime(String),
    #[error("dns process failed: {0}")]
    Process(String),
}

// ---------------------------------------------------------------------------
// DnsConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DnsConfig {
    pub data_dir: PathBuf,
    pub network: String,
    pub listen_addr: String,
}

impl DnsConfig {
    #[must_use]
    pub fn for_network(data_dir: &Path, network: &str, overlay_ip: OverlayIp) -> Self {
        let OverlayIp(ip) = overlay_ip;
        Self {
            data_dir: data_dir.to_path_buf(),
            network: network.to_string(),
            listen_addr: format!("[{ip}]:53"),
        }
    }

    pub fn from_env() -> Result<Self, DnsError> {
        let data_dir = match std::env::var_os("PLOYZ_DNS_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => {
                let aff = ployz_sdk::config::Affordances::detect();
                ployz_sdk::config::default_data_dir(&aff)
            }
        };
        let network = match std::env::var("PLOYZ_DNS_NETWORK") {
            Ok(network) if !network.trim().is_empty() => network,
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_NETWORK was set but empty".into(),
                ));
            }
            Err(_) => ployz_sdk::paths::read_active_network(&data_dir).ok_or_else(|| {
                DnsError::Config("no active network marker was found".into())
            })?,
        };
        let listen_addr = match std::env::var("PLOYZ_DNS_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => address,
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_LISTEN_ADDR is required for standalone mode".into(),
                ));
            }
        };

        Ok(Self {
            data_dir,
            network,
            listen_addr,
        })
    }
}

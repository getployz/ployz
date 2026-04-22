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
    pub metrics_listen_addr: Option<String>,
}

impl GatewayConfig {
    #[must_use]
    pub fn for_network(
        data_dir: &std::path::Path,
        network: &str,
        listen_addr: String,
        threads: usize,
        metrics_listen_addr: Option<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            network: network.to_string(),
            listen_addr,
            threads,
            metrics_listen_addr,
        }
    }

    pub fn from_env() -> Result<Self, GatewayError> {
        let data_dir = match std::env::var_os("PLOYZ_GATEWAY_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => ployz_config::default_data_dir(&host_paths_context()),
        };
        let network = match std::env::var("PLOYZ_GATEWAY_NETWORK") {
            Ok(network) if !network.trim().is_empty() => network,
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_NETWORK was set but empty".into(),
                ));
            }
            Err(_) => ployz_config::read_active_network(&data_dir)
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
        let metrics_listen_addr = match std::env::var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => Some(address),
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_METRICS_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => None,
        };

        Ok(Self {
            data_dir,
            network,
            listen_addr,
            threads,
            metrics_listen_addr,
        })
    }
}

fn host_paths_context() -> ployz_config::HostPathsContext {
    ployz_config::HostPathsContext {
        os: if cfg!(target_os = "linux") {
            ployz_config::Os::Linux
        } else if cfg!(target_os = "macos") {
            ployz_config::Os::Darwin
        } else {
            ployz_config::Os::Other
        },
        is_root: current_user_is_root(),
    }
}

fn current_user_is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` has no Rust-side preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LISTEN_ADDR, DEFAULT_THREADS, GatewayConfig};

    #[test]
    fn for_network_carries_metrics_listener() {
        let config = GatewayConfig::for_network(
            std::path::Path::new("/tmp/ployz"),
            "alpha",
            "0.0.0.0:80".into(),
            2,
            Some("127.0.0.1:9180".into()),
        );

        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9180")
        );
    }

    #[test]
    fn from_env_reads_metrics_listener() {
        unsafe {
            std::env::set_var("PLOYZ_GATEWAY_DATA_DIR", "/tmp/ployz-gateway");
            std::env::set_var("PLOYZ_GATEWAY_NETWORK", "alpha");
            std::env::set_var("PLOYZ_GATEWAY_LISTEN_ADDR", DEFAULT_LISTEN_ADDR);
            std::env::set_var("PLOYZ_GATEWAY_THREADS", DEFAULT_THREADS.to_string());
            std::env::set_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR", "127.0.0.1:9180");
        }

        let config = GatewayConfig::from_env().expect("gateway config should load");
        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9180")
        );

        unsafe {
            std::env::remove_var("PLOYZ_GATEWAY_DATA_DIR");
            std::env::remove_var("PLOYZ_GATEWAY_NETWORK");
            std::env::remove_var("PLOYZ_GATEWAY_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_GATEWAY_THREADS");
            std::env::remove_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR");
        }
    }
}

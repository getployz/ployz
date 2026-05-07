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
    pub machine_id: String,
    pub listen_addr: String,
    pub https_listen_addr: Option<String>,
    pub tls_cert_path: Option<PathBuf>,
    pub tls_key_path: Option<PathBuf>,
    pub threads: usize,
    pub metrics_listen_addr: Option<String>,
}

impl GatewayConfig {
    #[must_use]
    pub fn for_network(
        data_dir: &std::path::Path,
        network: &str,
        machine_id: String,
        listen_addr: String,
        https_listen_addr: Option<String>,
        tls_cert_path: Option<PathBuf>,
        tls_key_path: Option<PathBuf>,
        threads: usize,
        metrics_listen_addr: Option<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            network: network.to_string(),
            machine_id,
            listen_addr,
            https_listen_addr,
            tls_cert_path,
            tls_key_path,
            threads,
            metrics_listen_addr,
        }
    }

    pub fn from_env() -> Result<Self, GatewayError> {
        let data_dir = match std::env::var_os("PLOYZ_GATEWAY_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => ployz_config::default_data_dir(&ployz_config::detect_host_paths_context()),
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
        let machine_id = match std::env::var("PLOYZ_GATEWAY_MACHINE_ID") {
            Ok(machine_id) if !machine_id.trim().is_empty() => machine_id,
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_MACHINE_ID was set but empty".into(),
                ));
            }
            Err(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_MACHINE_ID must be set".into(),
                ));
            }
        };
        let threads = match std::env::var("PLOYZ_GATEWAY_THREADS") {
            Ok(raw) => raw.parse::<usize>().map_err(|err| {
                GatewayError::Config(format!(
                    "invalid PLOYZ_GATEWAY_THREADS value '{raw}': {err}"
                ))
            })?,
            Err(_) => DEFAULT_THREADS,
        };
        let https_listen_addr = match std::env::var("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => Some(address),
            Ok(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => None,
        };
        let tls_cert_path = match std::env::var_os("PLOYZ_GATEWAY_TLS_CERT_PATH") {
            Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
            Some(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_TLS_CERT_PATH was set but empty".into(),
                ));
            }
            None => None,
        };
        let tls_key_path = match std::env::var_os("PLOYZ_GATEWAY_TLS_KEY_PATH") {
            Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
            Some(_) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_TLS_KEY_PATH was set but empty".into(),
                ));
            }
            None => None,
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
        match (
            https_listen_addr.as_ref(),
            tls_cert_path.as_ref(),
            tls_key_path.as_ref(),
        ) {
            (None, None, None) | (Some(_), None, None) | (Some(_), Some(_), Some(_)) => {}
            (None, Some(_), _)
            | (None, _, Some(_))
            | (Some(_), Some(_), None)
            | (Some(_), None, Some(_)) => {
                return Err(GatewayError::Config(
                    "PLOYZ_GATEWAY_TLS_CERT_PATH and PLOYZ_GATEWAY_TLS_KEY_PATH must be set together with PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR; HTTPS may also use store-backed SNI certificates without static cert paths".into(),
                ));
            }
        }

        Ok(Self {
            data_dir,
            network,
            machine_id,
            listen_addr,
            https_listen_addr,
            tls_cert_path,
            tls_key_path,
            threads,
            metrics_listen_addr,
        })
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
            "machine-alpha".into(),
            "0.0.0.0:80".into(),
            None,
            None,
            None,
            2,
            Some("127.0.0.1:9180".into()),
        );

        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9180")
        );
        assert_eq!(config.machine_id, "machine-alpha");
    }

    #[test]
    fn from_env_reads_metrics_listener() {
        unsafe {
            std::env::set_var("PLOYZ_GATEWAY_DATA_DIR", "/tmp/ployz-gateway");
            std::env::set_var("PLOYZ_GATEWAY_NETWORK", "alpha");
            std::env::set_var("PLOYZ_GATEWAY_MACHINE_ID", "machine-alpha");
            std::env::set_var("PLOYZ_GATEWAY_LISTEN_ADDR", DEFAULT_LISTEN_ADDR);
            std::env::set_var("PLOYZ_GATEWAY_THREADS", DEFAULT_THREADS.to_string());
            std::env::set_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR", "127.0.0.1:9180");
            std::env::set_var("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR", "0.0.0.0:443");
            std::env::set_var("PLOYZ_GATEWAY_TLS_CERT_PATH", "/tmp/ployz-gateway/cert.pem");
            std::env::set_var("PLOYZ_GATEWAY_TLS_KEY_PATH", "/tmp/ployz-gateway/key.pem");
        }

        let config = GatewayConfig::from_env().expect("gateway config should load");
        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9180")
        );
        assert_eq!(config.machine_id, "machine-alpha");
        assert_eq!(config.https_listen_addr.as_deref(), Some("0.0.0.0:443"));
        assert_eq!(
            config.tls_cert_path.as_deref(),
            Some(std::path::Path::new("/tmp/ployz-gateway/cert.pem"))
        );
        assert_eq!(
            config.tls_key_path.as_deref(),
            Some(std::path::Path::new("/tmp/ployz-gateway/key.pem"))
        );

        unsafe {
            std::env::remove_var("PLOYZ_GATEWAY_DATA_DIR");
            std::env::remove_var("PLOYZ_GATEWAY_NETWORK");
            std::env::remove_var("PLOYZ_GATEWAY_MACHINE_ID");
            std::env::remove_var("PLOYZ_GATEWAY_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_GATEWAY_THREADS");
            std::env::remove_var("PLOYZ_GATEWAY_METRICS_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_GATEWAY_TLS_CERT_PATH");
            std::env::remove_var("PLOYZ_GATEWAY_TLS_KEY_PATH");
        }
    }
}

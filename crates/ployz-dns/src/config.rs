use std::path::{Path, PathBuf};

use thiserror::Error;

use ployz_model::OverlayIp;

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
    pub machine_id: String,
    pub overlay_listen_addr: String,
    pub bridge_listen_addr: Option<String>,
    pub metrics_listen_addr: Option<String>,
}

impl DnsConfig {
    #[must_use]
    pub fn for_network(
        data_dir: &Path,
        network: &str,
        machine_id: impl Into<String>,
        overlay_ip: OverlayIp,
        bridge_listen_addr: Option<String>,
        metrics_listen_addr: Option<String>,
    ) -> Self {
        let OverlayIp(ip) = overlay_ip;
        Self {
            data_dir: data_dir.to_path_buf(),
            network: network.to_string(),
            machine_id: machine_id.into(),
            overlay_listen_addr: format!("[{ip}]:53"),
            bridge_listen_addr,
            metrics_listen_addr,
        }
    }

    pub fn from_env() -> Result<Self, DnsError> {
        let data_dir = match std::env::var_os("PLOYZ_DNS_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => ployz_config::default_data_dir(&ployz_config::detect_host_paths_context()),
        };
        let network = match std::env::var("PLOYZ_DNS_NETWORK") {
            Ok(network) if !network.trim().is_empty() => network,
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_NETWORK was set but empty".into(),
                ));
            }
            Err(_) => ployz_config::read_active_network(&data_dir)
                .ok_or_else(|| DnsError::Config("no active network marker was found".into()))?,
        };
        let overlay_listen_addr = match std::env::var("PLOYZ_DNS_OVERLAY_LISTEN_ADDR")
            .or_else(|_| std::env::var("PLOYZ_DNS_LISTEN_ADDR"))
        {
            Ok(address) if !address.trim().is_empty() => address,
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_OVERLAY_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_OVERLAY_LISTEN_ADDR is required for standalone mode".into(),
                ));
            }
        };
        let machine_id = match std::env::var("PLOYZ_DNS_MACHINE_ID") {
            Ok(machine_id) if !machine_id.trim().is_empty() => machine_id,
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_MACHINE_ID was set but empty".into(),
                ));
            }
            Err(_) => {
                return Err(DnsError::Config("PLOYZ_DNS_MACHINE_ID is required".into()));
            }
        };
        let bridge_listen_addr = match std::env::var("PLOYZ_DNS_BRIDGE_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => Some(address),
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_BRIDGE_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => None,
        };
        let metrics_listen_addr = match std::env::var("PLOYZ_DNS_METRICS_LISTEN_ADDR") {
            Ok(address) if !address.trim().is_empty() => Some(address),
            Ok(_) => {
                return Err(DnsError::Config(
                    "PLOYZ_DNS_METRICS_LISTEN_ADDR was set but empty".into(),
                ));
            }
            Err(_) => None,
        };

        Ok(Self {
            data_dir,
            network,
            machine_id,
            overlay_listen_addr,
            bridge_listen_addr,
            metrics_listen_addr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::DnsConfig;
    use ployz_model::OverlayIp;
    use std::net::Ipv6Addr;
    use std::path::Path;

    #[test]
    fn for_network_sets_overlay_and_bridge_listeners() {
        let config = DnsConfig::for_network(
            Path::new("/tmp/ployz"),
            "default",
            "machine-alpha",
            OverlayIp(Ipv6Addr::LOCALHOST),
            Some("0.0.0.0:53".into()),
            Some("127.0.0.1:9153".into()),
        );

        assert_eq!(config.network, "default");
        assert_eq!(config.machine_id, "machine-alpha");
        assert_eq!(config.overlay_listen_addr, "[::1]:53");
        assert_eq!(config.bridge_listen_addr.as_deref(), Some("0.0.0.0:53"));
        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9153")
        );
    }

    #[test]
    fn for_network_allows_overlay_only_binding() {
        let config = DnsConfig::for_network(
            Path::new("/tmp/ployz"),
            "default",
            "machine-alpha",
            OverlayIp(Ipv6Addr::LOCALHOST),
            None,
            None,
        );

        assert_eq!(config.bridge_listen_addr, None);
        assert_eq!(config.metrics_listen_addr, None);
    }

    #[test]
    fn from_env_reads_metrics_listener() {
        unsafe {
            std::env::set_var("PLOYZ_DNS_DATA_DIR", "/tmp/ployz-dns");
            std::env::set_var("PLOYZ_DNS_NETWORK", "alpha");
            std::env::set_var("PLOYZ_DNS_MACHINE_ID", "machine-alpha");
            std::env::set_var("PLOYZ_DNS_OVERLAY_LISTEN_ADDR", "[::1]:53");
            std::env::set_var("PLOYZ_DNS_METRICS_LISTEN_ADDR", "127.0.0.1:9153");
        }

        let config = DnsConfig::from_env().expect("dns config should load");
        assert_eq!(
            config.metrics_listen_addr.as_deref(),
            Some("127.0.0.1:9153")
        );

        unsafe {
            std::env::remove_var("PLOYZ_DNS_DATA_DIR");
            std::env::remove_var("PLOYZ_DNS_NETWORK");
            std::env::remove_var("PLOYZ_DNS_MACHINE_ID");
            std::env::remove_var("PLOYZ_DNS_OVERLAY_LISTEN_ADDR");
            std::env::remove_var("PLOYZ_DNS_METRICS_LISTEN_ADDR");
        }
    }
}

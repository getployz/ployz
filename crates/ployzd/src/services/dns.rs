use crate::services::supervisor::{ServiceSupervision, SidecarHandle, SidecarSpec};
use async_trait::async_trait;
use ployz_dns::{DnsConfig, DnsError};
use ployz_runtime_api::RuntimeHandle;

// ---------------------------------------------------------------------------
// DnsHandle — supervision wrapper
// ---------------------------------------------------------------------------

enum DnsHandleInner {
    Noop,
    Sidecar(Box<SidecarHandle>),
}

pub struct DnsHandle {
    inner: DnsHandleInner,
}

impl DnsHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            inner: DnsHandleInner::Noop,
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), DnsError> {
        match &mut self.inner {
            DnsHandleInner::Noop => Ok(()),
            DnsHandleInner::Sidecar(handle) => handle
                .shutdown()
                .await
                .map_err(|e| DnsError::Process(e.to_string())),
        }
    }

    /// Detach from a running DNS service without stopping it.
    /// Docker and systemd services keep running so the daemon can restart.
    pub async fn detach(&mut self) -> Result<(), DnsError> {
        match &mut self.inner {
            DnsHandleInner::Noop => Ok(()),
            DnsHandleInner::Sidecar(handle) => handle
                .detach()
                .await
                .map_err(|e| DnsError::Process(e.to_string())),
        }
    }
}

#[async_trait]
impl RuntimeHandle for DnsHandle {
    async fn shutdown(mut self: Box<Self>) -> Result<(), String> {
        DnsHandle::shutdown(&mut self)
            .await
            .map_err(|error| error.to_string())
    }

    async fn detach(mut self: Box<Self>) -> Result<(), String> {
        DnsHandle::detach(&mut self)
            .await
            .map_err(|error| error.to_string())
    }
}

pub async fn start_managed_dns(
    supervision: Option<ServiceSupervision>,
    config: DnsConfig,
    image: &str,
) -> Result<DnsHandle, DnsError> {
    let Some(supervision) = supervision else {
        return Ok(DnsHandle::noop());
    };

    let spec = build_dns_sidecar_spec(&config, image);
    SidecarHandle::ensure(supervision, spec)
        .await
        .map(|handle| DnsHandle {
            inner: DnsHandleInner::Sidecar(Box::new(handle)),
        })
        .map_err(|e| DnsError::Process(e.to_string()))
}

fn build_dns_sidecar_spec(config: &DnsConfig, image: &str) -> SidecarSpec {
    let data_dir_str = config.data_dir.display().to_string();

    SidecarSpec {
        name: format!("dns-{}", config.network),
        image: image.to_string(),
        binary_name: "ployz-dns".to_string(),
        container_name: "ployz-dns".to_string(),
        cmd: vec!["ployz-dns".into()],
        env: {
            let mut env = vec![
                ("PLOYZ_DNS_DATA_DIR".into(), data_dir_str.clone()),
                ("PLOYZ_DNS_NETWORK".into(), config.network.clone()),
                (
                    "PLOYZ_DNS_OVERLAY_LISTEN_ADDR".into(),
                    config.overlay_listen_addr.clone(),
                ),
                (
                    "PLOYZ_DNS_LISTEN_ADDR".into(),
                    config.overlay_listen_addr.clone(),
                ),
            ];
            if let Some(bridge_listen_addr) = &config.bridge_listen_addr {
                env.push((
                    "PLOYZ_DNS_BRIDGE_LISTEN_ADDR".into(),
                    bridge_listen_addr.clone(),
                ));
            }
            if let Some(metrics_listen_addr) = &config.metrics_listen_addr {
                env.push((
                    "PLOYZ_DNS_METRICS_LISTEN_ADDR".into(),
                    metrics_listen_addr.clone(),
                ));
            }
            env
        },
        binds: vec![format!("{data_dir_str}:{data_dir_str}")],
        network_container: Some("ployz-networking".to_string()),
        systemd_extra: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::build_dns_sidecar_spec;
    use ployz_dns::DnsConfig;
    use ployz_types::model::OverlayIp;
    use std::net::Ipv6Addr;
    use std::path::Path;

    #[test]
    fn sidecar_spec_includes_metrics_env_when_configured() {
        let config = DnsConfig::for_network(
            Path::new("/tmp/ployz"),
            "alpha",
            OverlayIp(Ipv6Addr::LOCALHOST),
            Some("0.0.0.0:53".into()),
            Some("127.0.0.1:9153".into()),
        );

        let spec = build_dns_sidecar_spec(&config, "ployz-dns:latest");
        assert!(spec.env.iter().any(|(key, value)| {
            key == "PLOYZ_DNS_METRICS_LISTEN_ADDR" && value == "127.0.0.1:9153"
        }));
    }

    #[test]
    fn sidecar_spec_omits_metrics_env_by_default() {
        let config = DnsConfig::for_network(
            Path::new("/tmp/ployz"),
            "alpha",
            OverlayIp(Ipv6Addr::LOCALHOST),
            None,
            None,
        );

        let spec = build_dns_sidecar_spec(&config, "ployz-dns:latest");
        assert!(
            !spec
                .env
                .iter()
                .any(|(key, _)| key == "PLOYZ_DNS_METRICS_LISTEN_ADDR")
        );
    }
}

use crate::Mode;
use crate::services::supervisor::{SidecarHandle, SidecarSpec, SystemdType};

const DNS_IMAGE: &str = "ghcr.io/getployz/ployz-dns:latest";

// Re-export library types for public API consumers
pub use ployz_dns::{self as runtime, DnsConfig, DnsError, SharedDnsSnapshot};

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

pub async fn start_managed_dns(mode: Mode, config: DnsConfig) -> Result<DnsHandle, DnsError> {
    match mode {
        Mode::Memory => Ok(DnsHandle::noop()),
        Mode::Docker | Mode::HostExec | Mode::HostService => {
            let spec = build_dns_sidecar_spec(&config);
            SidecarHandle::ensure(mode, spec)
                .await
                .map(|handle| DnsHandle {
                    inner: DnsHandleInner::Sidecar(Box::new(handle)),
                })
                .map_err(|e| DnsError::Process(e.to_string()))
        }
    }
}

fn build_dns_sidecar_spec(config: &DnsConfig) -> SidecarSpec {
    let data_dir_str = config.data_dir.display().to_string();

    SidecarSpec {
        name: format!("dns-{}", config.network),
        image: DNS_IMAGE.to_string(),
        binary_name: "ployz-dns".to_string(),
        container_name: "ployz-dns".to_string(),
        cmd: vec!["ployz-dns".into()],
        env: vec![
            ("PLOYZ_DNS_DATA_DIR".into(), data_dir_str.clone()),
            ("PLOYZ_DNS_NETWORK".into(), config.network.clone()),
            ("PLOYZ_DNS_LISTEN_ADDR".into(), config.listen_addr.clone()),
        ],
        binds: vec![format!("{data_dir_str}:{data_dir_str}")],
        network_container: Some("ployz-networking".to_string()),
        compose_service: "dns".to_string(),
        systemd_type: SystemdType::Simple,
        systemd_extra: String::new(),
    }
}

use async_trait::async_trait;
use ployz_dns::DnsConfig;
use ployz_runtime_api::{Result as RuntimeResult, RuntimeError, RuntimeHandle};
use ployz_runtime_backends::sidecar::{ServiceSupervision, SidecarHandle, SidecarSpec};

pub struct DnsHandle {
    inner: Option<SidecarHandle>,
}

impl DnsHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self { inner: None }
    }
}

#[async_trait]
impl RuntimeHandle for DnsHandle {
    async fn shutdown(mut self: Box<Self>) -> RuntimeResult<()> {
        let Some(handle) = self.inner.as_mut() else {
            return Ok(());
        };
        handle.shutdown().await.map_err(|error| {
            RuntimeError::operation("managed service shutdown", format!("dns: {error}"))
        })
    }

    async fn detach(mut self: Box<Self>) -> RuntimeResult<()> {
        let Some(handle) = self.inner.as_mut() else {
            return Ok(());
        };
        handle.detach().await.map_err(|error| {
            RuntimeError::operation("managed service detach", format!("dns: {error}"))
        })
    }
}

pub async fn start_managed_dns(
    supervision: Option<ServiceSupervision>,
    config: DnsConfig,
    image: &str,
) -> RuntimeResult<DnsHandle> {
    let Some(supervision) = supervision else {
        return Ok(DnsHandle::noop());
    };

    let spec = build_dns_sidecar_spec(&config, image);
    SidecarHandle::ensure(supervision, spec)
        .await
        .map(|handle| DnsHandle {
            inner: Some(handle),
        })
        .map_err(|error| RuntimeError::operation("start dns", error.to_string()))
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
            env
        },
        binds: vec![format!("{data_dir_str}:{data_dir_str}")],
        network_container: Some("ployz-networking".to_string()),
        systemd_extra: String::new(),
    }
}

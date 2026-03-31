use crate::services::managed::ManagedServiceHandle;
use crate::services::supervisor::{ServiceSupervision, SidecarHandle, SidecarSpec};
use async_trait::async_trait;
use ployz_dns::DnsConfig;
use ployz_runtime_api::{Result as RuntimeResult, RuntimeError, RuntimeHandle};

pub struct DnsHandle {
    inner: ManagedServiceHandle,
}

impl DnsHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            inner: ManagedServiceHandle::noop(),
        }
    }
}

#[async_trait]
impl RuntimeHandle for DnsHandle {
    async fn shutdown(mut self: Box<Self>) -> RuntimeResult<()> {
        self.inner.shutdown("dns").await
    }

    async fn detach(mut self: Box<Self>) -> RuntimeResult<()> {
        self.inner.detach("dns").await
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
            inner: ManagedServiceHandle::sidecar(handle),
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

use tokio::sync::oneshot;
use tracing::warn;

use crate::drivers::StoreDriver;
use crate::sidecar::{SidecarHandle, SidecarSpec, SystemdType};
use crate::Mode;

const DNS_IMAGE: &str = "ghcr.io/getployz/ployz-dns:latest";

// Re-export library types for public API consumers
pub use ployz_dns::{self as runtime, DnsConfig, DnsError, SharedDnsSnapshot};

// ---------------------------------------------------------------------------
// DnsHandle — supervision wrapper
// ---------------------------------------------------------------------------

enum DnsHandleInner {
    Noop,
    Embedded(EmbeddedDnsHandle),
    Sidecar(SidecarHandle),
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
            DnsHandleInner::Embedded(handle) => handle.shutdown().await,
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
            DnsHandleInner::Embedded(handle) => handle.shutdown().await,
            DnsHandleInner::Sidecar(handle) => handle
                .detach()
                .await
                .map_err(|e| DnsError::Process(e.to_string())),
        }
    }
}

pub async fn start_managed_dns(
    mode: Mode,
    store: StoreDriver,
    config: DnsConfig,
) -> Result<DnsHandle, DnsError> {
    match mode {
        Mode::Memory => start_embedded_dns(store, config)
            .await
            .map(|handle| DnsHandle {
                inner: DnsHandleInner::Embedded(handle),
            }),
        Mode::Docker | Mode::HostExec | Mode::HostService => {
            let spec = build_dns_sidecar_spec(&config);
            SidecarHandle::start(mode, spec)
                .await
                .map(|handle| DnsHandle {
                    inner: DnsHandleInner::Sidecar(handle),
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
        compose_service: "dns".to_string(),
        systemd_type: SystemdType::Simple,
        systemd_extra: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Embedded mode — runs DNS server in-process via tokio task
// ---------------------------------------------------------------------------

struct EmbeddedDnsHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl EmbeddedDnsHandle {
    async fn shutdown(&mut self) -> Result<(), DnsError> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.await
            .map_err(|err| DnsError::Runtime(format!("dns task join failed: {err}")))?;
        Ok(())
    }
}

async fn start_embedded_dns(
    store: StoreDriver,
    config: DnsConfig,
) -> Result<EmbeddedDnsHandle, DnsError> {
    let state = ployz_dns::DnsStore::load_routing_state(&store).await?;
    let initial_snapshot = ployz_dns::project_dns(&state);
    let shared = SharedDnsSnapshot::new(initial_snapshot);
    ployz_dns::spawn_sync_thread_with_store(store, shared.clone())?;

    let listen_addr: std::net::SocketAddr = config
        .listen_addr
        .parse()
        .map_err(|err| DnsError::Config(format!("invalid listen_addr '{}': {err}", config.listen_addr)))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join = tokio::spawn(async move {
        if let Err(err) = ployz_dns::run_dns_server(listen_addr, shared, shutdown_rx).await {
            warn!(?err, "embedded dns server exited");
        }
    });

    Ok(EmbeddedDnsHandle {
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

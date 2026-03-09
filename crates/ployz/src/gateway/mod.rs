use std::path::PathBuf;

use tokio::sync::oneshot;
use tracing::warn;

use crate::drivers::StoreDriver;
use crate::sidecar::{SidecarHandle, SidecarSpec, SystemdType};
use crate::store::network::NetworkConfig;
use crate::Mode;

const GATEWAY_IMAGE: &str = "ghcr.io/getployz/ployz-gateway:latest";

// Re-export library types for public API consumers
pub use ployz_gateway::{
    self as runtime, GatewayApp, GatewayConfig, GatewayError, Opt, SharedSnapshot,
};

// ---------------------------------------------------------------------------
// GatewayHandle — supervision wrapper
// ---------------------------------------------------------------------------

enum GatewayHandleInner {
    Noop,
    Embedded(EmbeddedGatewayHandle),
    Sidecar(SidecarHandle),
}

pub struct GatewayHandle {
    inner: GatewayHandleInner,
}

impl GatewayHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            inner: GatewayHandleInner::Noop,
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), GatewayError> {
        match &mut self.inner {
            GatewayHandleInner::Noop => Ok(()),
            GatewayHandleInner::Embedded(handle) => handle.shutdown().await,
            GatewayHandleInner::Sidecar(handle) => handle
                .shutdown()
                .await
                .map_err(|e| GatewayError::Process(e.to_string())),
        }
    }

    /// Detach from a running gateway without stopping it.
    /// Docker and systemd containers keep running so the daemon can restart.
    pub async fn detach(&mut self) -> Result<(), GatewayError> {
        match &mut self.inner {
            GatewayHandleInner::Noop => Ok(()),
            GatewayHandleInner::Embedded(handle) => handle.shutdown().await,
            GatewayHandleInner::Sidecar(handle) => handle
                .detach()
                .await
                .map_err(|e| GatewayError::Process(e.to_string())),
        }
    }
}

pub async fn start_managed_gateway(
    mode: Mode,
    store: StoreDriver,
    config: GatewayConfig,
) -> Result<GatewayHandle, GatewayError> {
    match mode {
        Mode::Memory => start_embedded_gateway(store, config)
            .await
            .map(|handle| GatewayHandle {
                inner: GatewayHandleInner::Embedded(handle),
            }),
        Mode::Docker | Mode::HostExec | Mode::HostService => {
            let paths = GatewayPaths::for_config(&config);
            write_pingora_config(&paths, config.threads)?;

            let spec = build_gateway_sidecar_spec(&config, &paths);
            SidecarHandle::start(mode, spec)
                .await
                .map(|handle| GatewayHandle {
                    inner: GatewayHandleInner::Sidecar(handle),
                })
                .map_err(|e| GatewayError::Process(e.to_string()))
        }
    }
}

fn build_gateway_sidecar_spec(config: &GatewayConfig, paths: &GatewayPaths) -> SidecarSpec {
    let data_dir_str = config.data_dir.display().to_string();
    let gateway_dir_str = paths.gateway_dir.display().to_string();

    #[cfg(target_os = "linux")]
    let systemd_extra = {
        let pid_file = crate::sidecar::systemd_quote(&paths.pid_file.display().to_string());
        let pingora_config =
            crate::sidecar::systemd_quote(&paths.pingora_config.display().to_string());
        // Gateway uses forking mode with PIDFile, ExecReload for hot upgrades.
        // The binary path placeholder will be filled with the actual binary by sidecar.
        // We need to use find_binary here for ExecReload lines.
        let binary = crate::sidecar::find_binary("ployz-gateway")
            .map(|b| crate::sidecar::systemd_quote(&b.display().to_string()))
            .unwrap_or_default();
        format!(
            "PIDFile={pid_file}\nExecReload=/bin/kill -QUIT $MAINPID\nExecReload={binary} -u -d -c {pingora_config}\nExecStop=/bin/kill -TERM $MAINPID\n"
        )
    };
    #[cfg(not(target_os = "linux"))]
    let systemd_extra = String::new();

    SidecarSpec {
        name: format!("gateway-{}", config.network),
        image: GATEWAY_IMAGE.to_string(),
        binary_name: "ployz-gateway".to_string(),
        container_name: "ployz-gateway".to_string(),
        cmd: vec![
            "-c".into(),
            paths.pingora_config.display().to_string(),
        ],
        env: vec![
            ("PLOYZ_GATEWAY_DATA_DIR".into(), data_dir_str.clone()),
            ("PLOYZ_GATEWAY_NETWORK".into(), config.network.clone()),
            ("PLOYZ_GATEWAY_LISTEN_ADDR".into(), config.listen_addr.clone()),
            ("PLOYZ_GATEWAY_THREADS".into(), config.threads.to_string()),
        ],
        binds: vec![
            format!("{data_dir_str}:{data_dir_str}"),
            format!("{gateway_dir_str}:{gateway_dir_str}"),
        ],
        compose_service: "gateway".to_string(),
        systemd_type: SystemdType::Forking,
        systemd_extra,
    }
}

// ---------------------------------------------------------------------------
// Embedded mode — runs the proxy in-process
// ---------------------------------------------------------------------------

struct EmbeddedGatewayHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl EmbeddedGatewayHandle {
    async fn shutdown(&mut self) -> Result<(), GatewayError> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || join.join())
            .await
            .map_err(|err| GatewayError::Runtime(format!("gateway join failed: {err}")))?
            .map_err(|_| GatewayError::Runtime("gateway thread panicked".into()))?;
        Ok(())
    }
}

async fn start_embedded_gateway(
    store: StoreDriver,
    config: GatewayConfig,
) -> Result<EmbeddedGatewayHandle, GatewayError> {
    let initial_snapshot = ployz_gateway::load_projected_snapshot_from_store(&store).await?;
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);
    ployz_gateway::spawn_sync_thread_with_store(store, shared_snapshot.clone())?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let listen_addr = config.listen_addr.clone();
    let threads = config.threads;
    let join = std::thread::Builder::new()
        .name("ployz-gateway".into())
        .spawn(move || {
            #[cfg(unix)]
            let shutdown_signal = Box::new(ployz_gateway::EmbeddedShutdownWatch::new(shutdown_rx));
            #[cfg(not(unix))]
            let shutdown_signal = ();

            if let Err(err) = ployz_gateway::run_server(
                Opt::default(),
                listen_addr.as_str(),
                threads,
                shared_snapshot,
                #[cfg(unix)]
                Some(shutdown_signal),
                #[cfg(not(unix))]
                None,
            ) {
                warn!(?err, "embedded gateway exited");
            }
        })
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;

    Ok(EmbeddedGatewayHandle {
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

// ---------------------------------------------------------------------------
// Deployment artifact helpers
// ---------------------------------------------------------------------------

struct GatewayPaths {
    gateway_dir: PathBuf,
    pingora_config: PathBuf,
    pid_file: PathBuf,
    upgrade_sock: PathBuf,
}

impl GatewayPaths {
    fn for_config(config: &GatewayConfig) -> Self {
        let gateway_dir =
            NetworkConfig::dir(&config.data_dir, &config.network).join("gateway");
        Self {
            pingora_config: gateway_dir.join("pingora.yaml"),
            pid_file: gateway_dir.join("pingora.pid"),
            upgrade_sock: gateway_dir.join("pingora.sock"),
            gateway_dir,
        }
    }
}

fn write_pingora_config(paths: &GatewayPaths, threads: usize) -> Result<(), GatewayError> {
    std::fs::create_dir_all(&paths.gateway_dir)
        .map_err(|err| GatewayError::Process(format!("create gateway dir: {err}")))?;
    let contents = format!(
        "---\nversion: 1\nthreads: {threads}\npid_file: {}\nupgrade_sock: {}\n",
        paths.pid_file.display(),
        paths.upgrade_sock.display(),
    );
    std::fs::write(&paths.pingora_config, contents)
        .map_err(|err| GatewayError::Process(format!("write gateway config: {err}")))?;
    Ok(())
}

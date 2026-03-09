use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::{info, warn};

use crate::drivers::StoreDriver;
use crate::store::network::NetworkConfig;
use crate::Mode;

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

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
    Child(HostGatewayHandle),
    Systemd(SystemdGatewayHandle),
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
            GatewayHandleInner::Child(handle) => handle.shutdown().await,
            GatewayHandleInner::Systemd(handle) => handle.shutdown().await,
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
        Mode::Docker | Mode::HostExec => start_host_gateway(config).await.map(|handle| {
            GatewayHandle {
                inner: GatewayHandleInner::Child(handle),
            }
        }),
        Mode::HostService => start_systemd_gateway(config)
            .await
            .map(|handle| GatewayHandle {
                inner: GatewayHandleInner::Systemd(handle),
            }),
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
// Host mode — spawns the binary as a child process
// ---------------------------------------------------------------------------

struct HostGatewayHandle {
    child: AsyncMutex<Option<Child>>,
}

impl HostGatewayHandle {
    async fn shutdown(&self) -> Result<(), GatewayError> {
        let mut guard = self.child.lock().await;
        let Some(child) = guard.as_mut() else {
            return Ok(());
        };

        let pid = child.id();
        #[cfg(unix)]
        if let Some(raw_pid) = pid {
            unsafe {
                libc::kill(raw_pid as i32, libc::SIGTERM);
            }
            match tokio::time::timeout(STOP_GRACE_PERIOD, child.wait()).await {
                Ok(Ok(_status)) => {
                    guard.take();
                    return Ok(());
                }
                Ok(Err(err)) => {
                    warn!(?err, "gateway wait after SIGTERM failed, force killing");
                }
                Err(_) => {
                    warn!(
                        pid = raw_pid,
                        "gateway did not exit after SIGTERM, force killing"
                    );
                }
            }
        }

        child.kill().await.map_err(|err| {
            GatewayError::Process(format!("failed to kill gateway pid {pid:?}: {err}"))
        })?;
        let _ = child.wait().await.map_err(|err| {
            GatewayError::Process(format!("failed to wait for gateway pid {pid:?}: {err}"))
        })?;
        guard.take();
        Ok(())
    }
}

async fn start_host_gateway(config: GatewayConfig) -> Result<HostGatewayHandle, GatewayError> {
    let binary = find_gateway_binary()?;
    let paths = GatewayPaths::for_config(&config);
    write_pingora_config(&paths, config.threads)?;

    let child = Command::new(&binary)
        .arg("-c")
        .arg(&paths.pingora_config)
        .env("PLOYZ_GATEWAY_DATA_DIR", &config.data_dir)
        .env("PLOYZ_GATEWAY_NETWORK", &config.network)
        .env("PLOYZ_GATEWAY_LISTEN_ADDR", &config.listen_addr)
        .env("PLOYZ_GATEWAY_THREADS", config.threads.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            GatewayError::Process(format!("failed to spawn {}: {err}", binary.display()))
        })?;

    info!(
        pid = child.id(),
        binary = %binary.display(),
        network = %config.network,
        listen = %config.listen_addr,
        "gateway started"
    );

    Ok(HostGatewayHandle {
        child: AsyncMutex::new(Some(child)),
    })
}

// ---------------------------------------------------------------------------
// Systemd mode — installs and manages a systemd unit
// ---------------------------------------------------------------------------

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SystemdGatewayHandle {
    unit_name: String,
}

impl SystemdGatewayHandle {
    async fn shutdown(&self) -> Result<(), GatewayError> {
        #[cfg(target_os = "linux")]
        {
            run_systemctl(["stop", self.unit_name.as_str()]).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(GatewayError::Process(
                "systemd-managed gateway is only supported on Linux".into(),
            ))
        }
    }
}

async fn start_systemd_gateway(
    config: GatewayConfig,
) -> Result<SystemdGatewayHandle, GatewayError> {
    #[cfg(target_os = "linux")]
    {
        let binary = find_gateway_binary()?;
        let paths = GatewayPaths::for_config(&config);
        write_pingora_config(&paths, config.threads)?;
        write_systemd_unit(&paths, &binary, &config)?;
        run_systemctl(["daemon-reload"]).await?;
        run_systemctl(["start", paths.unit_name.as_str()]).await?;
        Ok(SystemdGatewayHandle {
            unit_name: paths.unit_name,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(GatewayError::Process(
            "systemd-managed gateway is only supported on Linux".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Deployment artifact helpers
// ---------------------------------------------------------------------------

struct GatewayPaths {
    gateway_dir: PathBuf,
    pingora_config: PathBuf,
    pid_file: PathBuf,
    upgrade_sock: PathBuf,
    #[cfg(target_os = "linux")]
    unit_name: String,
    #[cfg(target_os = "linux")]
    unit_path: PathBuf,
}

impl GatewayPaths {
    fn for_config(config: &GatewayConfig) -> Self {
        let gateway_dir =
            NetworkConfig::dir(&config.data_dir, &config.network).join("gateway");
        #[cfg(target_os = "linux")]
        let unit_stem = sanitize_unit_component(&config.network);
        #[cfg(target_os = "linux")]
        let unit_name = format!("ployz-gateway-{unit_stem}.service");
        Self {
            pingora_config: gateway_dir.join("pingora.yaml"),
            pid_file: gateway_dir.join("pingora.pid"),
            upgrade_sock: gateway_dir.join("pingora.sock"),
            #[cfg(target_os = "linux")]
            unit_path: PathBuf::from("/etc/systemd/system").join(&unit_name),
            #[cfg(target_os = "linux")]
            unit_name,
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

#[cfg(target_os = "linux")]
fn write_systemd_unit(
    paths: &GatewayPaths,
    binary: &Path,
    config: &GatewayConfig,
) -> Result<(), GatewayError> {
    let binary = systemd_quote(&binary.display().to_string());
    let pingora_config = systemd_quote(&paths.pingora_config.display().to_string());
    let pid_file = systemd_quote(&paths.pid_file.display().to_string());
    let data_dir = systemd_quote(&config.data_dir.display().to_string());
    let network = systemd_quote(&config.network);
    let listen_addr = systemd_quote(&config.listen_addr);
    let threads = systemd_quote(&config.threads.to_string());
    let unit = format!(
        "[Unit]\nDescription=Ployz gateway for network {}\nAfter=network-online.target\n\n[Service]\nType=forking\nPIDFile={pid_file}\nEnvironment=\"PLOYZ_GATEWAY_DATA_DIR={data_dir}\"\nEnvironment=\"PLOYZ_GATEWAY_NETWORK={network}\"\nEnvironment=\"PLOYZ_GATEWAY_LISTEN_ADDR={listen_addr}\"\nEnvironment=\"PLOYZ_GATEWAY_THREADS={threads}\"\nExecStart={binary} -d -c {pingora_config}\nExecReload=/bin/kill -QUIT $MAINPID\nExecReload={binary} -u -d -c {pingora_config}\nExecStop=/bin/kill -TERM $MAINPID\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n",
        config.network
    );
    std::fs::write(&paths.unit_path, unit)
        .map_err(|err| GatewayError::Process(format!("write systemd unit: {err}")))?;
    Ok(())
}

#[cfg(target_os = "linux")]
async fn run_systemctl<const N: usize>(args: [&str; N]) -> Result<(), GatewayError> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .await
        .map_err(|err| GatewayError::Process(format!("systemctl failed to start: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(GatewayError::Process(format!(
        "systemctl {} failed: {}",
        args.join(" "),
        stderr.trim()
    )))
}

fn find_gateway_binary() -> Result<PathBuf, GatewayError> {
    let current_exe = std::env::current_exe()
        .map_err(|err| GatewayError::Process(format!("current_exe failed: {err}")))?;
    let candidates = [
        current_exe.with_file_name("ployz-gateway"),
        PathBuf::from("/usr/local/bin/ployz-gateway"),
        PathBuf::from("/usr/bin/ployz-gateway"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(GatewayError::Process(
        "ployz-gateway binary not found".into(),
    ))
}

#[cfg(target_os = "linux")]
fn sanitize_unit_component(network: &str) -> String {
    let sanitized = network
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".into()
    } else {
        sanitized
    }
}

#[cfg(target_os = "linux")]
fn systemd_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

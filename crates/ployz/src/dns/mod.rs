use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::{info, warn};

use crate::drivers::StoreDriver;
use crate::Mode;

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

// Re-export library types for public API consumers
pub use ployz_dns::{self as runtime, DnsConfig, DnsError, SharedDnsSnapshot};

// ---------------------------------------------------------------------------
// DnsHandle — supervision wrapper
// ---------------------------------------------------------------------------

enum DnsHandleInner {
    Noop,
    Embedded(EmbeddedDnsHandle),
    Child(HostDnsHandle),
    Systemd(SystemdDnsHandle),
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
            DnsHandleInner::Child(handle) => handle.shutdown().await,
            DnsHandleInner::Systemd(handle) => handle.shutdown().await,
        }
    }
}

pub async fn start_managed_dns(
    mode: Mode,
    store: StoreDriver,
    config: DnsConfig,
) -> Result<DnsHandle, DnsError> {
    match mode {
        Mode::Memory => start_embedded_dns(store, config).await.map(|handle| DnsHandle {
            inner: DnsHandleInner::Embedded(handle),
        }),
        Mode::Docker | Mode::HostExec => start_host_dns(config).await.map(|handle| DnsHandle {
            inner: DnsHandleInner::Child(handle),
        }),
        Mode::HostService => start_systemd_dns(config).await.map(|handle| DnsHandle {
            inner: DnsHandleInner::Systemd(handle),
        }),
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

// ---------------------------------------------------------------------------
// Host mode — spawns the binary as a child process
// ---------------------------------------------------------------------------

struct HostDnsHandle {
    child: AsyncMutex<Option<Child>>,
}

impl HostDnsHandle {
    async fn shutdown(&self) -> Result<(), DnsError> {
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
                    warn!(?err, "dns wait after SIGTERM failed, force killing");
                }
                Err(_) => {
                    warn!(
                        pid = raw_pid,
                        "dns did not exit after SIGTERM, force killing"
                    );
                }
            }
        }

        child.kill().await.map_err(|err| {
            DnsError::Process(format!("failed to kill dns pid {pid:?}: {err}"))
        })?;
        let _ = child.wait().await.map_err(|err| {
            DnsError::Process(format!("failed to wait for dns pid {pid:?}: {err}"))
        })?;
        guard.take();
        Ok(())
    }
}

async fn start_host_dns(config: DnsConfig) -> Result<HostDnsHandle, DnsError> {
    let binary = find_dns_binary()?;

    let child = Command::new(&binary)
        .env("PLOYZ_DNS_DATA_DIR", &config.data_dir)
        .env("PLOYZ_DNS_NETWORK", &config.network)
        .env("PLOYZ_DNS_LISTEN_ADDR", &config.listen_addr)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            DnsError::Process(format!("failed to spawn {}: {err}", binary.display()))
        })?;

    info!(
        pid = child.id(),
        binary = %binary.display(),
        network = %config.network,
        listen = %config.listen_addr,
        "dns started"
    );

    Ok(HostDnsHandle {
        child: AsyncMutex::new(Some(child)),
    })
}

// ---------------------------------------------------------------------------
// Systemd mode — installs and manages a systemd unit
// ---------------------------------------------------------------------------

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SystemdDnsHandle {
    unit_name: String,
}

impl SystemdDnsHandle {
    async fn shutdown(&self) -> Result<(), DnsError> {
        #[cfg(target_os = "linux")]
        {
            run_systemctl(["stop", self.unit_name.as_str()]).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(DnsError::Process(
                "systemd-managed dns is only supported on Linux".into(),
            ))
        }
    }
}

async fn start_systemd_dns(config: DnsConfig) -> Result<SystemdDnsHandle, DnsError> {
    #[cfg(target_os = "linux")]
    {
        let binary = find_dns_binary()?;
        let unit_name = format!(
            "ployz-dns-{}.service",
            sanitize_unit_component(&config.network)
        );
        let unit_path = PathBuf::from("/etc/systemd/system").join(&unit_name);

        let data_dir = systemd_quote(&config.data_dir.display().to_string());
        let network = systemd_quote(&config.network);
        let listen_addr = systemd_quote(&config.listen_addr);
        let binary_str = systemd_quote(&binary.display().to_string());
        let unit = format!(
            "[Unit]\nDescription=Ployz DNS for network {}\nAfter=network-online.target\n\n[Service]\nType=simple\nEnvironment=\"PLOYZ_DNS_DATA_DIR={data_dir}\"\nEnvironment=\"PLOYZ_DNS_NETWORK={network}\"\nEnvironment=\"PLOYZ_DNS_LISTEN_ADDR={listen_addr}\"\nExecStart={binary_str}\nRestart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n",
            config.network,
        );
        std::fs::write(&unit_path, unit)
            .map_err(|err| DnsError::Process(format!("write systemd unit: {err}")))?;
        run_systemctl(["daemon-reload"]).await?;
        run_systemctl(["start", unit_name.as_str()]).await?;
        Ok(SystemdDnsHandle { unit_name })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(DnsError::Process(
            "systemd-managed dns is only supported on Linux".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
async fn run_systemctl<const N: usize>(args: [&str; N]) -> Result<(), DnsError> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .await
        .map_err(|err| DnsError::Process(format!("systemctl failed to start: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(DnsError::Process(format!(
        "systemctl {} failed: {}",
        args.join(" "),
        stderr.trim()
    )))
}

fn find_dns_binary() -> Result<PathBuf, DnsError> {
    let current_exe = std::env::current_exe()
        .map_err(|err| DnsError::Process(format!("current_exe failed: {err}")))?;
    let candidates = [
        current_exe.with_file_name("ployz-dns"),
        PathBuf::from("/usr/local/bin/ployz-dns"),
        PathBuf::from("/usr/bin/ployz-dns"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(DnsError::Process("ployz-dns binary not found".into()))
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

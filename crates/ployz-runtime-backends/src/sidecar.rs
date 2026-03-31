use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

use crate::runtime::labels::build_system_labels;
use crate::runtime::{ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSupervision {
    DockerContainer,
    ChildProcess,
    Systemd,
}

pub struct SidecarSpec {
    pub name: String,
    pub image: String,
    pub binary_name: String,
    pub container_name: String,
    pub cmd: Vec<String>,
    pub env: Vec<(String, String)>,
    pub binds: Vec<String>,
    pub network_container: Option<String>,
    pub systemd_extra: String,
}

fn sidecar_to_runtime_spec(spec: &SidecarSpec) -> RuntimeContainerSpec {
    let key = format!("system/{}", spec.name);
    let network_mode = spec
        .network_container
        .as_ref()
        .map(|name| format!("container:{name}"));
    let labels = build_system_labels(&key, None);

    RuntimeContainerSpec {
        key,
        container_name: spec.container_name.clone(),
        image: spec.image.clone(),
        pull_policy: PullPolicy::Always,
        cmd: Some(spec.cmd.clone()),
        entrypoint: None,
        env: spec.env.clone(),
        labels,
        binds: spec.binds.clone(),
        tmpfs: std::collections::HashMap::new(),
        dns_servers: Vec::new(),
        network_mode,
        port_bindings: None,
        exposed_ports: None,
        cap_add: Vec::new(),
        cap_drop: Vec::new(),
        privileged: false,
        user: None,
        restart_policy: None,
        memory_bytes: None,
        nano_cpus: None,
        sysctls: std::collections::HashMap::new(),
        stop_timeout: None,
        pid_mode: None,
    }
}

pub struct SidecarHandle {
    inner: SidecarInner,
}

enum SidecarInner {
    Child(ChildHandle),
    Docker(DockerHandle),
    Systemd(SystemdHandle),
}

impl SidecarHandle {
    pub async fn ensure(
        supervision: ServiceSupervision,
        spec: SidecarSpec,
    ) -> Result<Self, SidecarError> {
        match supervision {
            ServiceSupervision::DockerContainer => ensure_docker(spec).await.map(|handle| Self {
                inner: SidecarInner::Docker(handle),
            }),
            ServiceSupervision::ChildProcess => start_child(spec).await.map(|handle| Self {
                inner: SidecarInner::Child(handle),
            }),
            ServiceSupervision::Systemd => ensure_systemd(spec).await.map(|handle| Self {
                inner: SidecarInner::Systemd(handle),
            }),
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), SidecarError> {
        match &mut self.inner {
            SidecarInner::Child(handle) => handle.shutdown().await,
            SidecarInner::Docker(handle) => handle.shutdown().await,
            SidecarInner::Systemd(handle) => handle.shutdown().await,
        }
    }

    pub async fn detach(&mut self) -> Result<(), SidecarError> {
        match &mut self.inner {
            SidecarInner::Docker(_) | SidecarInner::Systemd(_) => Ok(()),
            SidecarInner::Child(handle) => handle.shutdown().await,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("sidecar process error: {0}")]
    Process(String),
}

struct ChildHandle {
    name: String,
    child: AsyncMutex<Option<Child>>,
}

impl ChildHandle {
    async fn shutdown(&self) -> Result<(), SidecarError> {
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
                    warn!(?err, name = %self.name, "wait after SIGTERM failed, force killing");
                }
                Err(_) => {
                    warn!(
                        pid = raw_pid,
                        name = %self.name,
                        "did not exit after SIGTERM, force killing"
                    );
                }
            }
        }

        child.kill().await.map_err(|err| {
            SidecarError::Process(format!("failed to kill {} pid {pid:?}: {err}", self.name))
        })?;
        let _ = child.wait().await.map_err(|err| {
            SidecarError::Process(format!(
                "failed to wait for {} pid {pid:?}: {err}",
                self.name
            ))
        })?;
        guard.take();
        Ok(())
    }
}

async fn start_child(spec: SidecarSpec) -> Result<ChildHandle, SidecarError> {
    let binary = find_binary(&spec.binary_name)?;

    let mut cmd = Command::new(&binary);
    for arg in &spec.cmd {
        cmd.arg(arg);
    }
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            SidecarError::Process(format!("failed to spawn {}: {err}", binary.display()))
        })?;

    info!(
        pid = child.id(),
        binary = %binary.display(),
        name = %spec.name,
        "{} started",
        spec.name,
    );

    Ok(ChildHandle {
        name: spec.name,
        child: AsyncMutex::new(Some(child)),
    })
}

struct DockerHandle {
    engine: ContainerEngine,
    container_name: String,
    service_name: String,
}

impl DockerHandle {
    async fn shutdown(&self) -> Result<(), SidecarError> {
        self.engine
            .remove(&self.container_name, STOP_GRACE_PERIOD)
            .await
            .map_err(|error| {
                SidecarError::Process(format!("docker remove {}: {error}", self.service_name))
            })
    }
}

async fn ensure_docker(spec: SidecarSpec) -> Result<DockerHandle, SidecarError> {
    let engine = ContainerEngine::connect()
        .await
        .map_err(|error| SidecarError::Process(error.to_string()))?;

    let runtime_spec = sidecar_to_runtime_spec(&spec);
    let result = engine
        .ensure(&runtime_spec)
        .await
        .map_err(|error| SidecarError::Process(format!("{}: {error}", spec.name)))?;

    match &result.action {
        EnsureAction::Adopted => {
            info!(name = %spec.container_name, "adopted existing container");
        }
        EnsureAction::Created => {
            info!(name = %spec.container_name, image = %spec.image, "{} container created", spec.name);
        }
        EnsureAction::Recreated { changed } => {
            info!(
                name = %spec.container_name,
                image = %spec.image,
                changed = ?changed,
                "{} container recreated",
                spec.name,
            );
        }
    }

    Ok(DockerHandle {
        engine,
        container_name: spec.container_name,
        service_name: spec.name,
    })
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SystemdHandle {
    unit_name: String,
    service_name: String,
}

impl SystemdHandle {
    async fn shutdown(&self) -> Result<(), SidecarError> {
        #[cfg(target_os = "linux")]
        {
            run_systemctl(["stop", self.unit_name.as_str()], &self.service_name).await
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(SidecarError::Process(format!(
                "systemd-managed {} is only supported on Linux",
                self.service_name
            )))
        }
    }
}

#[cfg(target_os = "linux")]
fn build_unit_content(spec: &SidecarSpec, binary: &std::path::Path) -> String {
    let binary_str = systemd_quote(&binary.display().to_string());

    let env_lines: String = spec
        .env
        .iter()
        .map(|(key, value)| {
            format!(
                "Environment=\"{}={}\"",
                systemd_quote(key),
                systemd_quote(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cmd_args = spec
        .cmd
        .iter()
        .map(|arg| systemd_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");

    let exec_start = if cmd_args.is_empty() {
        binary_str
    } else {
        format!("{binary_str} {cmd_args}")
    };

    format!(
        "[Unit]\nDescription=Ployz {name}\nAfter=network-online.target\n\n[Service]\nType=simple\n{env_lines}\nExecStart={exec_start}\n{extra}Restart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n",
        name = spec.name,
        extra = spec.systemd_extra,
    )
}

async fn ensure_systemd(spec: SidecarSpec) -> Result<SystemdHandle, SidecarError> {
    #[cfg(target_os = "linux")]
    {
        let binary = find_binary(&spec.binary_name)?;
        let unit_stem = sanitize_unit_component(&spec.name);
        let unit_name = format!("ployz-{unit_stem}.service");
        let unit_path = PathBuf::from("/etc/systemd/system").join(&unit_name);

        let desired_unit = build_unit_content(&spec, &binary);
        let existing_unit = std::fs::read_to_string(&unit_path).ok();
        if existing_unit.as_deref() == Some(&desired_unit) {
            let output = tokio::process::Command::new("systemctl")
                .args(["is-active", unit_name.as_str()])
                .output()
                .await
                .ok();
            let is_active = output
                .as_ref()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "active")
                .unwrap_or(false);
            if is_active {
                info!(unit = %unit_name, "adopted existing systemd service");
                return Ok(SystemdHandle {
                    unit_name,
                    service_name: spec.name,
                });
            }
        }

        std::fs::write(&unit_path, desired_unit)
            .map_err(|err| SidecarError::Process(format!("write systemd unit: {err}")))?;
        run_systemctl(["daemon-reload"], &spec.name).await?;
        run_systemctl(["restart", unit_name.as_str()], &spec.name).await?;
        Ok(SystemdHandle {
            unit_name,
            service_name: spec.name,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = spec;
        Err(SidecarError::Process(
            "systemd-managed sidecars are only supported on Linux".into(),
        ))
    }
}

pub(crate) fn find_binary(name: &str) -> Result<PathBuf, SidecarError> {
    let current_exe = std::env::current_exe()
        .map_err(|err| SidecarError::Process(format!("current_exe failed: {err}")))?;
    let candidates = [
        current_exe.with_file_name(name),
        PathBuf::from(format!("/usr/local/bin/{name}")),
        PathBuf::from(format!("/usr/bin/{name}")),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(SidecarError::Process(format!("{name} binary not found")))
}

#[cfg(target_os = "linux")]
async fn run_systemctl<const N: usize>(
    args: [&str; N],
    service_name: &str,
) -> Result<(), SidecarError> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .await
        .map_err(|err| {
            SidecarError::Process(format!(
                "systemctl failed to start for {service_name}: {err}"
            ))
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(SidecarError::Process(format!(
        "systemctl {} failed for {service_name}: {}",
        args.join(" "),
        stderr.trim()
    )))
}

#[cfg(target_os = "linux")]
pub(crate) fn sanitize_unit_component(name: &str) -> String {
    let sanitized = name
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
pub(crate) fn systemd_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

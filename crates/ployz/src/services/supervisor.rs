use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);
const LABEL_PLOYZ_CONFIG_HASH: &str = "ployz.config-hash";
const LABEL_PLOYZ_PARENT_ID: &str = "ployz.parent-container-id";

// ---------------------------------------------------------------------------
// SidecarSpec — declarative description of a sidecar service
// ---------------------------------------------------------------------------

/// What kind of systemd unit type to use.
pub enum SystemdType {
    Simple,
    Forking,
}

/// Declarative description of a sidecar service.
pub struct SidecarSpec {
    pub name: String,
    pub image: String,
    pub binary_name: String,
    pub container_name: String,
    pub cmd: Vec<String>,
    pub env: Vec<(String, String)>,
    pub binds: Vec<String>,
    /// Container whose network namespace to share (Docker `--network=container:X`).
    pub network_container: Option<String>,
    pub compose_service: String,
    pub systemd_type: SystemdType,
    /// Extra unit file content (PIDFile, ExecReload, etc.) inserted into [Service].
    pub systemd_extra: String,
}

impl SidecarSpec {
    /// Stable hash of the fields that determine whether a running service can be adopted.
    fn config_hash(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.image.hash(&mut hasher);
        self.cmd.hash(&mut hasher);
        self.env.hash(&mut hasher);
        self.binds.hash(&mut hasher);
        self.network_container.hash(&mut hasher);
        self.systemd_extra.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

// ---------------------------------------------------------------------------
// SidecarHandle — lifecycle wrapper
// ---------------------------------------------------------------------------

pub struct SidecarHandle {
    inner: SidecarInner,
}

enum SidecarInner {
    Child(ChildHandle),
    Docker(DockerHandle),
    Systemd(SystemdHandle),
}

impl SidecarHandle {
    /// Ensure the service is running, adopting an existing instance when possible.
    ///
    /// - **Docker**: inspects the existing container; adopts if running, config hash
    ///   matches, and parent network container (if any) is unchanged. Otherwise
    ///   tears down the old container and creates a fresh one.
    /// - **HostService** (systemd): adopts if the unit is active and its file content
    ///   matches the desired spec. Otherwise rewrites + restarts.
    /// - **HostExec**: always spawns a new child process (no persistent state to adopt).
    pub async fn ensure(mode: crate::Mode, spec: SidecarSpec) -> Result<Self, SidecarError> {
        match mode {
            crate::Mode::Memory => unreachable!("memory mode does not use sidecars"),
            crate::Mode::Docker => ensure_docker(spec).await.map(|h| Self {
                inner: SidecarInner::Docker(h),
            }),
            crate::Mode::HostExec => start_child(spec).await.map(|h| Self {
                inner: SidecarInner::Child(h),
            }),
            crate::Mode::HostService => ensure_systemd(spec).await.map(|h| Self {
                inner: SidecarInner::Systemd(h),
            }),
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), SidecarError> {
        match &mut self.inner {
            SidecarInner::Child(h) => h.shutdown().await,
            SidecarInner::Docker(h) => h.shutdown().await,
            SidecarInner::Systemd(h) => h.shutdown().await,
        }
    }

    /// Detach without stopping. Docker/systemd keep running across daemon restarts.
    pub async fn detach(&mut self) -> Result<(), SidecarError> {
        match &mut self.inner {
            SidecarInner::Docker(_) | SidecarInner::Systemd(_) => Ok(()),
            SidecarInner::Child(h) => h.shutdown().await,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("sidecar process error: {0}")]
    Process(String),
}

// ---------------------------------------------------------------------------
// Child (HostExec) handle
// ---------------------------------------------------------------------------

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
            SidecarError::Process(format!(
                "failed to kill {} pid {pid:?}: {err}",
                self.name
            ))
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
            SidecarError::Process(format!(
                "failed to spawn {}: {err}",
                binary.display()
            ))
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

// ---------------------------------------------------------------------------
// Docker handle
// ---------------------------------------------------------------------------

struct DockerHandle {
    docker: Docker,
    container_name: String,
    service_name: String,
}

impl DockerHandle {
    async fn shutdown(&self) -> Result<(), SidecarError> {
        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();
        match self
            .docker
            .stop_container(&self.container_name, Some(stop_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => {}
            Err(e) => {
                return Err(SidecarError::Process(format!(
                    "docker stop {}: {e}",
                    self.service_name
                )));
            }
        }

        let remove_opts = RemoveContainerOptionsBuilder::default().build();
        match self
            .docker
            .remove_container(&self.container_name, Some(remove_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                return Err(SidecarError::Process(format!(
                    "docker remove {}: {e}",
                    self.service_name
                )));
            }
        }

        info!(name = %self.container_name, "{} container stopped and removed", self.service_name);
        Ok(())
    }
}

/// Resolve the Docker container ID for a container name, or `None` if not found.
async fn resolve_container_id(docker: &Docker, name: &str) -> Option<String> {
    docker
        .inspect_container(name, None)
        .await
        .ok()
        .and_then(|info| info.id)
}

/// Check whether an existing container can be adopted (running + identity match).
async fn try_adopt_docker(
    docker: &Docker,
    spec: &SidecarSpec,
    config_hash: &str,
) -> Option<DockerHandle> {
    let info = docker
        .inspect_container(&spec.container_name, None)
        .await
        .ok()?;

    let is_running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
    if !is_running {
        return None;
    }

    let labels = info
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref());

    // Config hash must match.
    let stored_hash = labels.and_then(|l| l.get(LABEL_PLOYZ_CONFIG_HASH));
    if stored_hash.map(String::as_str) != Some(config_hash) {
        info!(name = %spec.container_name, "config drift detected, recreating");
        return None;
    }

    // If sharing a network namespace, the parent container must be the same instance.
    if let Some(ref parent_name) = spec.network_container {
        let stored_parent = labels.and_then(|l| l.get(LABEL_PLOYZ_PARENT_ID));
        let current_parent = resolve_container_id(docker, parent_name).await;
        match (stored_parent, current_parent.as_deref()) {
            (Some(stored), Some(current)) if stored == current => {}
            _ => {
                info!(
                    name = %spec.container_name,
                    parent = %parent_name,
                    "parent container changed, recreating"
                );
                return None;
            }
        }
    }

    info!(name = %spec.container_name, "adopted existing container");
    Some(DockerHandle {
        docker: docker.clone(),
        container_name: spec.container_name.clone(),
        service_name: spec.name.clone(),
    })
}

async fn ensure_docker(spec: SidecarSpec) -> Result<DockerHandle, SidecarError> {
    let docker = Docker::connect_with_socket_defaults()
        .map_err(|e| SidecarError::Process(format!("docker connect: {e}")))?;

    docker
        .ping()
        .await
        .map_err(|e| SidecarError::Process(format!("docker ping: {e}")))?;

    let config_hash = spec.config_hash();

    // Try to adopt an existing, matching container.
    if let Some(handle) = try_adopt_docker(&docker, &spec, &config_hash).await {
        return Ok(handle);
    }

    // Best-effort pull.
    let (repo, tag) = match spec.image.split_once(':') {
        Some((r, t)) => (r, t),
        None => (spec.image.as_str(), "latest"),
    };
    let pull_opts = CreateImageOptionsBuilder::default()
        .from_image(repo)
        .tag(tag)
        .build();
    let mut stream = docker.create_image(Some(pull_opts), None, None);
    while let Some(result) = stream.next().await {
        match result {
            Ok(info) => {
                if let Some(status) = info.status {
                    info!(image = %spec.image, %status, "pulling");
                }
            }
            Err(e) => {
                warn!(?e, image = %spec.image, "pull failed, trying cached image");
                break;
            }
        }
    }

    // Remove existing container.
    let remove_opts = RemoveContainerOptionsBuilder::default()
        .force(true)
        .build();
    if let Err(e) = docker
        .remove_container(&spec.container_name, Some(remove_opts))
        .await
        && !matches!(
            e,
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }
        )
    {
        warn!(?e, name = %spec.container_name, "failed to remove existing {} container", spec.name);
    }

    let network_mode = spec
        .network_container
        .as_ref()
        .map(|name| format!("container:{name}"));

    let host_config = HostConfig {
        binds: Some(spec.binds.clone()),
        network_mode,
        ..Default::default()
    };

    let mut labels: HashMap<String, String> = [
        ("com.docker.compose.project".into(), "ployz-system".into()),
        (
            "com.docker.compose.service".into(),
            spec.compose_service.clone(),
        ),
        (LABEL_PLOYZ_CONFIG_HASH.into(), config_hash),
    ]
    .into_iter()
    .collect();

    // Track parent container ID so future ensure() calls can detect netns drift.
    if let Some(ref parent_name) = spec.network_container
        && let Some(parent_id) = resolve_container_id(&docker, parent_name).await
    {
        labels.insert(LABEL_PLOYZ_PARENT_ID.into(), parent_id);
    }

    let env: Vec<String> = spec
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let container_config = ContainerCreateBody {
        image: Some(spec.image.clone()),
        cmd: Some(spec.cmd.clone()),
        env: Some(env),
        labels: Some(labels),
        host_config: Some(host_config),
        ..Default::default()
    };

    let create_opts = CreateContainerOptionsBuilder::default()
        .name(&spec.container_name)
        .build();

    docker
        .create_container(Some(create_opts), container_config)
        .await
        .map_err(|e| SidecarError::Process(format!("docker create {}: {e}", spec.name)))?;

    docker
        .start_container(&spec.container_name, None)
        .await
        .map_err(|e| SidecarError::Process(format!("docker start {}: {e}", spec.name)))?;

    info!(
        name = %spec.container_name,
        image = %spec.image,
        "{} container started",
        spec.name,
    );

    Ok(DockerHandle {
        docker,
        container_name: spec.container_name,
        service_name: spec.name,
    })
}

// ---------------------------------------------------------------------------
// Systemd handle
// ---------------------------------------------------------------------------

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

/// Build the desired unit file content for a sidecar spec.
#[cfg(target_os = "linux")]
fn build_unit_content(spec: &SidecarSpec, binary: &std::path::Path) -> String {
    let binary_str = systemd_quote(&binary.display().to_string());

    let systemd_type_str = match spec.systemd_type {
        SystemdType::Simple => "simple",
        SystemdType::Forking => "forking",
    };

    let env_lines: String = spec
        .env
        .iter()
        .map(|(k, v)| {
            format!(
                "Environment=\"{}={}\"",
                systemd_quote(k),
                systemd_quote(v)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cmd_args = spec
        .cmd
        .iter()
        .map(|a| systemd_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    let exec_start = if cmd_args.is_empty() {
        binary_str
    } else {
        format!("{binary_str} {cmd_args}")
    };

    format!(
        "[Unit]\nDescription=Ployz {name}\nAfter=network-online.target\n\n[Service]\nType={systemd_type_str}\n{env_lines}\nExecStart={exec_start}\n{extra}Restart=on-failure\n\n[Install]\nWantedBy=multi-user.target\n",
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

        // Try to adopt: if the unit file matches and the service is active, skip restart.
        let existing_unit = std::fs::read_to_string(&unit_path).ok();
        if existing_unit.as_deref() == Some(&desired_unit) {
            let output = tokio::process::Command::new("systemctl")
                .args(["is-active", unit_name.as_str()])
                .output()
                .await
                .ok();
            let is_active = output
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
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

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub fn find_binary(name: &str) -> Result<PathBuf, SidecarError> {
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
    Err(SidecarError::Process(format!(
        "{name} binary not found"
    )))
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
pub fn sanitize_unit_component(name: &str) -> String {
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
pub fn systemd_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

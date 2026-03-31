use std::path::PathBuf;

use crate::mesh_state::network::NetworkConfig;
use async_trait::async_trait;
use ployz_gateway::{GatewayConfig, GatewayError};
use ployz_runtime_api::{Result as RuntimeResult, RuntimeError, RuntimeHandle};
use ployz_runtime_backends::sidecar::{ServiceSupervision, SidecarHandle, SidecarSpec};

pub struct GatewayHandle {
    inner: Option<SidecarHandle>,
}

impl GatewayHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self { inner: None }
    }
}

#[async_trait]
impl RuntimeHandle for GatewayHandle {
    async fn shutdown(mut self: Box<Self>) -> RuntimeResult<()> {
        let Some(handle) = self.inner.as_mut() else {
            return Ok(());
        };
        handle.shutdown().await.map_err(|error| {
            RuntimeError::operation("managed service shutdown", format!("gateway: {error}"))
        })
    }

    async fn detach(mut self: Box<Self>) -> RuntimeResult<()> {
        let Some(handle) = self.inner.as_mut() else {
            return Ok(());
        };
        handle.detach().await.map_err(|error| {
            RuntimeError::operation("managed service detach", format!("gateway: {error}"))
        })
    }
}

pub async fn start_managed_gateway(
    supervision: Option<ServiceSupervision>,
    config: GatewayConfig,
    image: &str,
) -> RuntimeResult<GatewayHandle> {
    let Some(supervision) = supervision else {
        return Ok(GatewayHandle::noop());
    };

    let paths = GatewayPaths::for_config(&config);
    write_pingora_config(&paths, config.threads)
        .map_err(|error| RuntimeError::operation("write gateway config", error.to_string()))?;

    let spec = build_gateway_sidecar_spec(&config, &paths, image);
    SidecarHandle::ensure(supervision, spec)
        .await
        .map(|handle| GatewayHandle {
            inner: Some(handle),
        })
        .map_err(|error| RuntimeError::operation("start gateway", error.to_string()))
}

fn build_gateway_sidecar_spec(
    config: &GatewayConfig,
    paths: &GatewayPaths,
    image: &str,
) -> SidecarSpec {
    let data_dir_str = config.data_dir.display().to_string();
    let gateway_dir_str = paths.gateway_dir.display().to_string();

    #[cfg(target_os = "linux")]
    let systemd_extra = {
        let pid_file = quote_systemd_arg(&paths.pid_file.display().to_string());
        let pingora_config = quote_systemd_arg(&paths.pingora_config.display().to_string());
        // The gateway stays attached to the invoking process for normal startup,
        // so the systemd unit must be `Type=simple`. Reload still uses Pingora's
        // upgrade path and keeps the PID file/socket metadata available.
        let binary = find_gateway_binary()
            .map(|binary| quote_systemd_arg(&binary.display().to_string()))
            .unwrap_or_default();
        format!(
            "PIDFile={pid_file}\nExecReload=/bin/kill -QUIT $MAINPID\nExecReload={binary} -u -d -c {pingora_config}\nExecStop=/bin/kill -TERM $MAINPID\n"
        )
    };
    #[cfg(not(target_os = "linux"))]
    let systemd_extra = String::new();

    SidecarSpec {
        name: format!("gateway-{}", config.network),
        image: image.to_string(),
        binary_name: "ployz-gateway".to_string(),
        container_name: "ployz-gateway".to_string(),
        cmd: vec!["-c".into(), paths.pingora_config.display().to_string()],
        env: vec![
            ("PLOYZ_GATEWAY_DATA_DIR".into(), data_dir_str.clone()),
            ("PLOYZ_GATEWAY_NETWORK".into(), config.network.clone()),
            (
                "PLOYZ_GATEWAY_LISTEN_ADDR".into(),
                config.listen_addr.clone(),
            ),
            ("PLOYZ_GATEWAY_THREADS".into(), config.threads.to_string()),
        ],
        binds: vec![
            format!("{data_dir_str}:{data_dir_str}"),
            format!("{gateway_dir_str}:{gateway_dir_str}"),
        ],
        network_container: Some("ployz-networking".to_string()),
        systemd_extra,
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
}

impl GatewayPaths {
    fn for_config(config: &GatewayConfig) -> Self {
        let gateway_dir = NetworkConfig::dir(&config.data_dir, &config.network).join("gateway");
        Self {
            pingora_config: gateway_dir.join("pingora.yaml"),
            pid_file: gateway_dir.join("pingora.pid"),
            upgrade_sock: gateway_dir.join("pingora.sock"),
            gateway_dir,
        }
    }
}

fn write_pingora_config(
    paths: &GatewayPaths,
    threads: usize,
) -> std::result::Result<(), GatewayError> {
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
fn find_gateway_binary() -> std::result::Result<std::path::PathBuf, GatewayError> {
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
fn quote_systemd_arg(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

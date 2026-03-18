use std::path::PathBuf;

use async_trait::async_trait;
use crate::services::supervisor::{ServiceSupervision, SidecarHandle, SidecarSpec, SystemdType};
use ployz_gateway::{GatewayConfig, GatewayError};
use ployz_runtime_api::RuntimeHandle;
use ployz_state::store::network::NetworkConfig;

// ---------------------------------------------------------------------------
// GatewayHandle — supervision wrapper
// ---------------------------------------------------------------------------

enum GatewayHandleInner {
    Noop,
    Sidecar(Box<SidecarHandle>),
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
            GatewayHandleInner::Sidecar(handle) => handle
                .detach()
                .await
                .map_err(|e| GatewayError::Process(e.to_string())),
        }
    }
}

#[async_trait]
impl RuntimeHandle for GatewayHandle {
    async fn shutdown(mut self: Box<Self>) -> Result<(), String> {
        GatewayHandle::shutdown(&mut self)
            .await
            .map_err(|error| error.to_string())
    }

    async fn detach(mut self: Box<Self>) -> Result<(), String> {
        GatewayHandle::detach(&mut self)
            .await
            .map_err(|error| error.to_string())
    }
}

pub async fn start_managed_gateway(
    supervision: Option<ServiceSupervision>,
    config: GatewayConfig,
    image: &str,
) -> Result<GatewayHandle, GatewayError> {
    let Some(supervision) = supervision else {
        return Ok(GatewayHandle::noop());
    };

    let paths = GatewayPaths::for_config(&config);
    write_pingora_config(&paths, config.threads)?;

    let spec = build_gateway_sidecar_spec(&config, &paths, image);
    SidecarHandle::ensure(supervision, spec)
        .await
        .map(|handle| GatewayHandle {
            inner: GatewayHandleInner::Sidecar(Box::new(handle)),
        })
        .map_err(|e| GatewayError::Process(e.to_string()))
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
        let pid_file =
            crate::services::supervisor::systemd_quote(&paths.pid_file.display().to_string());
        let pingora_config =
            crate::services::supervisor::systemd_quote(&paths.pingora_config.display().to_string());
        // The gateway stays attached to the invoking process for normal startup,
        // so the systemd unit must be `Type=simple`. Reload still uses Pingora's
        // upgrade path and keeps the PID file/socket metadata available.
        let binary = crate::services::supervisor::find_binary("ployz-gateway")
            .map(|b| crate::services::supervisor::systemd_quote(&b.display().to_string()))
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
        compose_service: "gateway".to_string(),
        systemd_type: SystemdType::Simple,
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

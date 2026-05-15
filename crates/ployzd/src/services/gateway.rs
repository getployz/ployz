use std::path::PathBuf;

use crate::mesh_state::network::NetworkConfig;
use async_trait::async_trait;
use ployz_gateway::{GatewayConfig, GatewayError};
use ployz_node_runtime::sidecar::{ServiceSupervision, SidecarHandle, SidecarSpec};
#[cfg(target_os = "linux")]
use ployz_node_runtime::sidecar::{find_binary, systemd_quote};
use ployz_runtime_api::RuntimeHandle;

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
        let pid_file = systemd_quote(&paths.pid_file.display().to_string());
        let pingora_config = systemd_quote(&paths.pingora_config.display().to_string());
        // The gateway stays attached to the invoking process for normal startup,
        // so the systemd unit must be `Type=simple`. Reload still uses Pingora's
        // upgrade path and keeps the PID file/socket metadata available.
        let binary = find_binary("ployz-gateway")
            .map(|b| systemd_quote(&b.display().to_string()))
            .unwrap_or_default();
        format!(
            "PIDFile={pid_file}\nExecReload=/bin/kill -QUIT $MAINPID\nExecReload={binary} -u -d -c {pingora_config}\nExecStop=/bin/kill -TERM $MAINPID\n"
        )
    };
    #[cfg(not(target_os = "linux"))]
    let systemd_extra = String::new();

    SidecarSpec {
        name: format!("gateway-{}", config.network),
        version: env!("CARGO_PKG_VERSION").to_string(),
        image: image.to_string(),
        binary_name: "ployz-gateway".to_string(),
        container_name: "ployz-gateway".to_string(),
        cmd: vec!["-c".into(), paths.pingora_config.display().to_string()],
        env: vec![
            ("PLOYZ_GATEWAY_DATA_DIR".into(), data_dir_str.clone()),
            ("PLOYZ_GATEWAY_NETWORK".into(), config.network.clone()),
            ("PLOYZ_GATEWAY_MACHINE_ID".into(), config.machine_id.clone()),
            (
                "PLOYZ_GATEWAY_LISTEN_ADDR".into(),
                config.listen_addr.clone(),
            ),
            ("PLOYZ_GATEWAY_THREADS".into(), config.threads.to_string()),
        ]
        .into_iter()
        .chain(config.https_listen_addr.iter().map(|listen_addr| {
            (
                "PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR".into(),
                listen_addr.clone(),
            )
        }))
        .chain(config.tls_cert_path.iter().map(|path| {
            (
                "PLOYZ_GATEWAY_TLS_CERT_PATH".into(),
                path.display().to_string(),
            )
        }))
        .chain(config.tls_key_path.iter().map(|path| {
            (
                "PLOYZ_GATEWAY_TLS_KEY_PATH".into(),
                path.display().to_string(),
            )
        }))
        .chain(config.metrics_listen_addr.iter().map(|listen_addr| {
            (
                "PLOYZ_GATEWAY_METRICS_LISTEN_ADDR".into(),
                listen_addr.clone(),
            )
        }))
        .collect(),
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

#[cfg(test)]
mod tests {
    use super::{GatewayPaths, build_gateway_sidecar_spec};
    use ployz_gateway::GatewayConfig;
    use std::path::Path;

    #[test]
    fn sidecar_spec_includes_metrics_env_when_configured() {
        let config = GatewayConfig::for_network(
            Path::new("/tmp/ployz"),
            "alpha",
            "founder".into(),
            "0.0.0.0:80".into(),
            None,
            None,
            None,
            2,
            Some("127.0.0.1:9180".into()),
        );
        let paths = GatewayPaths::for_config(&config);

        let spec = build_gateway_sidecar_spec(&config, &paths, "ployz-gateway:latest");
        assert!(
            spec.env
                .iter()
                .any(|(key, value)| { key == "PLOYZ_GATEWAY_MACHINE_ID" && value == "founder" })
        );
        assert!(spec.env.iter().any(|(key, value)| {
            key == "PLOYZ_GATEWAY_METRICS_LISTEN_ADDR" && value == "127.0.0.1:9180"
        }));
    }

    #[test]
    fn sidecar_spec_omits_metrics_env_by_default() {
        let config = GatewayConfig::for_network(
            Path::new("/tmp/ployz"),
            "alpha",
            "founder".into(),
            "0.0.0.0:80".into(),
            None,
            None,
            None,
            2,
            None,
        );
        let paths = GatewayPaths::for_config(&config);

        let spec = build_gateway_sidecar_spec(&config, &paths, "ployz-gateway:latest");
        assert!(
            !spec
                .env
                .iter()
                .any(|(key, _)| key == "PLOYZ_GATEWAY_METRICS_LISTEN_ADDR")
        );
    }
}

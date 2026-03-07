use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
};
use futures_util::StreamExt;
use ipnet::Ipv4Net;
use tracing::{info, warn};

use crate::error::{Error, Result};

const CTL_BIN: &str = "ployz-ebpf-ctl";

pub struct ContainerDataplane {
    docker: Docker,
    container_name: String,
    bridge_ifname: String,
}

impl ContainerDataplane {
    pub async fn attach(
        container_name: &str,
        image: &str,
        bridge_ifname: &str,
    ) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        let dp = Self {
            docker,
            container_name: container_name.to_string(),
            bridge_ifname: bridge_ifname.to_string(),
        };

        // Start the sidecar container
        dp.start_sidecar(image).await?;

        // Exec: attach TC classifiers
        dp.exec(&[CTL_BIN, "attach", bridge_ifname]).await?;

        info!(
            bridge = bridge_ifname,
            container = container_name,
            "eBPF TC classifiers attached (container)"
        );
        Ok(dp)
    }

    pub async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        let subnet_str = subnet.to_string();
        let ifindex_str = ifindex.to_string();
        self.exec(&[CTL_BIN, "route", "add", &subnet_str, &ifindex_str])
            .await?;
        info!(%subnet, ifindex, "eBPF route upserted (container)");
        Ok(())
    }

    pub async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        let subnet_str = subnet.to_string();
        match self.exec(&[CTL_BIN, "route", "del", &subnet_str]).await {
            Ok(()) => info!(%subnet, "eBPF route removed (container)"),
            Err(e) => warn!(%subnet, ?e, "eBPF route remove failed"),
        }
        Ok(())
    }

    pub async fn detach(self) -> Result<()> {
        let _ = self.exec(&[CTL_BIN, "detach", &self.bridge_ifname]).await;
        info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (container)");
        self.stop_sidecar().await
    }

    async fn exec(&self, cmd: &[&str]) -> Result<()> {
        let exec = self
            .docker
            .create_exec(
                &self.container_name,
                CreateExecOptions::<String> {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| Error::operation("ebpf exec create", e.to_string()))?;

        let exec_id = exec.id.clone();
        let mut stderr_buf = String::new();

        match self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| Error::operation("ebpf exec start", e.to_string()))?
        {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(result) = output.next().await {
                    match result {
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            if stderr_buf.len() < 4096 {
                                stderr_buf.push_str(&String::from_utf8_lossy(&message));
                                stderr_buf.truncate(4096);
                            }
                        }
                        Err(e) => {
                            return Err(Error::operation("ebpf exec", e.to_string()));
                        }
                        _ => {}
                    }
                }

                let inspect = self
                    .docker
                    .inspect_exec(&exec_id)
                    .await
                    .map_err(|e| Error::operation("ebpf exec inspect", e.to_string()))?;

                if let Some(code) = inspect.exit_code {
                    if code != 0 {
                        let detail = if stderr_buf.is_empty() {
                            format!("exit code {code}")
                        } else {
                            format!("exit code {code}: {}", stderr_buf.trim())
                        };
                        return Err(Error::operation("ebpf exec", detail));
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        Ok(())
    }

    async fn start_sidecar(&self, image: &str) -> Result<()> {
        // Pull image (best-effort)
        let (repo, tag) = match image.split_once(':') {
            Some((r, t)) => (r, t),
            None => (image, "latest"),
        };
        let opts = CreateImageOptionsBuilder::default()
            .from_image(repo)
            .tag(tag)
            .build();
        let mut stream = self.docker.create_image(Some(opts), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        info!(image, %status, "pulling ebpf sidecar");
                    }
                }
                Err(e) => {
                    warn!(?e, image, "ebpf sidecar pull failed, trying cached");
                    break;
                }
            }
        }

        // Remove existing
        let rm_opts = RemoveContainerOptionsBuilder::default().force(true).build();
        let _ = self
            .docker
            .remove_container(&self.container_name, Some(rm_opts))
            .await;

        // Create with privileged + pid:host + WG container's network namespace
        let host_config = HostConfig {
            privileged: Some(true),
            pid_mode: Some("host".to_string()),
            network_mode: Some("container:ployz-wireguard".to_string()),
            ..Default::default()
        };

        let labels: std::collections::HashMap<String, String> = [
            ("com.docker.compose.project".into(), "ployz-system".into()),
            ("com.docker.compose.service".into(), "ebpf".into()),
        ]
        .into_iter()
        .collect();

        let config = ContainerCreateBody {
            image: Some(image.to_string()),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        let create_opts = CreateContainerOptionsBuilder::default()
            .name(&self.container_name)
            .build();

        self.docker
            .create_container(Some(create_opts), config)
            .await
            .map_err(|e| Error::operation("ebpf container create", e.to_string()))?;

        self.docker
            .start_container(&self.container_name, None)
            .await
            .map_err(|e| Error::operation("ebpf container start", e.to_string()))?;

        // Give the container a moment to stabilize, then verify it's running.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let inspect = self
            .docker
            .inspect_container(&self.container_name, None)
            .await
            .map_err(|e| Error::operation("ebpf container inspect", e.to_string()))?;

        let running = inspect
            .state
            .as_ref()
            .and_then(|s| s.running)
            .unwrap_or(false);

        if !running {
            let exit_code = inspect.state.as_ref().and_then(|s| s.exit_code);
            let error = inspect
                .state
                .as_ref()
                .and_then(|s| s.error.clone())
                .unwrap_or_default();
            return Err(Error::operation(
                "ebpf container start",
                format!(
                    "container exited immediately (exit_code={exit_code:?}, error={error:?})"
                ),
            ));
        }

        info!(
            name = %self.container_name,
            image,
            "eBPF sidecar container started"
        );
        Ok(())
    }

    async fn stop_sidecar(&self) -> Result<()> {
        let rm_opts = RemoveContainerOptionsBuilder::default().force(true).build();
        match self
            .docker
            .remove_container(&self.container_name, Some(rm_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(Error::operation("ebpf container remove", e.to_string())),
        }

        info!(name = %self.container_name, "eBPF sidecar container stopped");
        Ok(())
    }
}

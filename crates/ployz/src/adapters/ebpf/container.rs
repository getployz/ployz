use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use ipnet::Ipv4Net;
use tracing::{info, warn};

use crate::error::{Error, Result};

const CTL_BIN: &str = "/usr/local/bin/ployz-dataplane";

/// eBPF dataplane that execs `ployz-dataplane` inside the WireGuard container.
/// The WG container image includes the dataplane binary, so no separate
/// sidecar is needed — just docker exec.
pub struct ContainerDataplane {
    docker: Docker,
    container_name: String,
    bridge_ifname: String,
}

impl ContainerDataplane {
    pub async fn attach(
        wg_container_name: &str,
        bridge_ifname: &str,
        wg_ifname: &str,
    ) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        let dp = Self {
            docker,
            container_name: wg_container_name.to_string(),
            bridge_ifname: bridge_ifname.to_string(),
        };

        // Exec: attach TC classifiers inside the WG container
        dp.exec(&[CTL_BIN, "attach", bridge_ifname, wg_ifname])
            .await?;

        info!(
            bridge = bridge_ifname,
            container = wg_container_name,
            "eBPF TC classifiers attached (via WG container)"
        );
        Ok(dp)
    }

    pub async fn set_observe(&self, enabled: bool) -> Result<()> {
        let state = if enabled { "on" } else { "off" };
        self.exec(&[CTL_BIN, "observe", state]).await?;
        info!(enabled, "eBPF observation toggled (container)");
        Ok(())
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
        let _ = self
            .exec(&[CTL_BIN, "detach", &self.bridge_ifname])
            .await;
        info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (container)");
        Ok(())
    }

    async fn exec(&self, cmd: &[&str]) -> Result<()> {
        // The WG container has its own network namespace (for wg0), but eBPF TC
        // classifiers must attach to the bridge in the host netns. Since the
        // container runs with pid_mode=host, nsenter into PID 1's netns.
        let mut full_cmd = vec![
            "nsenter".to_string(),
            "--net=/proc/1/ns/net".to_string(),
            "--".to_string(),
        ];
        full_cmd.extend(cmd.iter().map(|s| s.to_string()));

        let exec = self
            .docker
            .create_exec(
                &self.container_name,
                CreateExecOptions::<String> {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    privileged: Some(true),
                    cmd: Some(full_cmd),
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
}

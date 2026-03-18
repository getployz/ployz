use crate::error::{Error, Result};
use ipnet::Ipv4Net;
use ployz_runtime_api::ObserveMode;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{info, warn};

const CTL_BIN: &str = "/usr/local/bin/ployz-bpfctl";

pub struct ContainerDataplane {
    container_name: String,
    bridge_ifname: String,
}

impl ContainerDataplane {
    pub async fn attach(
        wg_container_name: &str,
        bridge_ifname: &str,
        wg_ifname: &str,
    ) -> Result<Self> {
        let dataplane = Self {
            container_name: wg_container_name.to_string(),
            bridge_ifname: bridge_ifname.to_string(),
        };

        dataplane
            .exec(&[CTL_BIN, "attach", bridge_ifname, wg_ifname])
            .await?;

        info!(
            bridge = bridge_ifname,
            container = wg_container_name,
            "eBPF TC classifiers attached (via WG container)"
        );
        Ok(dataplane)
    }

    pub async fn set_observe(&self, mode: ObserveMode) -> Result<()> {
        let state = match mode {
            ObserveMode::Enabled => "on",
            ObserveMode::Disabled => "off",
        };
        self.exec(&[CTL_BIN, "observe", state]).await?;
        info!(?mode, "eBPF observation toggled (container)");
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
            Err(error) => warn!(%subnet, ?error, "eBPF route remove failed"),
        }
        Ok(())
    }

    pub async fn detach(self) -> Result<()> {
        let _ = self.exec(&[CTL_BIN, "detach", &self.bridge_ifname]).await;
        info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (container)");
        Ok(())
    }

    pub async fn detach_ref(&self) -> Result<()> {
        let _ = self.exec(&[CTL_BIN, "detach", &self.bridge_ifname]).await;
        info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (container)");
        Ok(())
    }

    async fn exec(&self, cmd: &[&str]) -> Result<()> {
        let mut full_cmd: Vec<&str> = vec![
            "exec",
            "--privileged",
            &self.container_name,
            "nsenter",
            "--net=/proc/1/ns/net",
            "--",
        ];
        full_cmd.extend_from_slice(cmd);

        let output = Command::new("docker")
            .args(&full_cmd)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| Error::operation("ebpf exec", error.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("exit code {}", output.status)
            } else {
                stderr
            };
            return Err(Error::operation("ebpf exec", detail));
        }

        Ok(())
    }
}

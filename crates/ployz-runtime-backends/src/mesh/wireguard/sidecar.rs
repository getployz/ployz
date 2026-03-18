use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use std::net::Ipv4Addr;
use tracing::info;

use super::docker::{docker_exec_capture, docker_force_remove};
use crate::error::{Error, Result};

use super::PERSISTENT_KEEPALIVE_SECS;
use super::config::encode_key;

const INTERFACE_NAME: &str = "wg0";
const DEFAULT_MTU: u16 = 1420;

pub struct SidecarConfig {
    pub container_name: String,
    pub private_key: [u8; 32],
    pub overlay_ip: Ipv4Addr,
    pub backbone_pubkey: [u8; 32],
    pub backbone_endpoint: String,
    pub cluster_cidr: String,
    pub image: String,
}

pub struct WgSidecar {
    docker: Docker,
    config: SidecarConfig,
}

impl WgSidecar {
    #[must_use]
    pub fn new(docker: Docker, config: SidecarConfig) -> Self {
        Self { docker, config }
    }

    #[must_use]
    pub fn container_name(&self) -> &str {
        &self.config.container_name
    }

    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(self.config.private_key))
            .to_bytes()
    }

    pub async fn up(&self) -> Result<()> {
        // Remove any stale container
        self.remove_existing().await;

        let host_config = HostConfig {
            cap_add: Some(vec!["NET_ADMIN".to_string()]),
            ..Default::default()
        };

        let labels: std::collections::HashMap<String, String> = [
            ("com.docker.compose.project".into(), "ployz-system".into()),
            (
                "com.docker.compose.service".into(),
                format!(
                    "sidecar-{}",
                    self.config
                        .container_name
                        .trim_start_matches("ployz-sidecar-")
                ),
            ),
        ]
        .into_iter()
        .collect();

        let config = ContainerCreateBody {
            image: Some(self.config.image.clone()),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        let options = CreateContainerOptionsBuilder::default()
            .name(&self.config.container_name)
            .build();

        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| Error::operation("sidecar create", e.to_string()))?;

        self.docker
            .start_container(&self.config.container_name, None)
            .await
            .map_err(|e| Error::operation("sidecar start", e.to_string()))?;

        info!(name = %self.config.container_name, "sidecar container started");
        Ok(())
    }

    pub async fn setup_interface(&self) -> Result<()> {
        let mtu = DEFAULT_MTU.to_string();

        self.exec(&["ip", "link", "add", INTERFACE_NAME, "type", "wireguard"])
            .await?;

        self.exec(&["ip", "link", "set", INTERFACE_NAME, "mtu", &mtu])
            .await?;

        // Write private key to a temp file inside the container
        let privkey_b64 = encode_key(&self.config.private_key);
        self.exec(&[
            "sh",
            "-c",
            &format!("echo '{privkey_b64}' > /tmp/wg-private.key"),
        ])
        .await?;

        let backbone_pubkey_b64 = encode_key(&self.config.backbone_pubkey);
        let keepalive_secs = PERSISTENT_KEEPALIVE_SECS.to_string();
        self.exec(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "private-key",
            "/tmp/wg-private.key",
            "peer",
            &backbone_pubkey_b64,
            "endpoint",
            &self.config.backbone_endpoint,
            "allowed-ips",
            &format!("{},fd00::/8", self.config.cluster_cidr),
            "persistent-keepalive",
            &keepalive_secs,
        ])
        .await?;

        // Clean up private key file
        self.exec(&["rm", "/tmp/wg-private.key"]).await?;

        let overlay_addr = format!("{}/32", self.config.overlay_ip);
        self.exec(&["ip", "addr", "add", &overlay_addr, "dev", INTERFACE_NAME])
            .await?;

        self.exec(&["ip", "link", "set", INTERFACE_NAME, "up"])
            .await?;

        // Route overlay traffic through WG
        self.exec(&[
            "ip",
            "route",
            "add",
            &self.config.cluster_cidr,
            "dev",
            INTERFACE_NAME,
        ])
        .await?;

        self.exec(&[
            "ip",
            "-6",
            "route",
            "add",
            "fd00::/8",
            "dev",
            INTERFACE_NAME,
        ])
        .await?;

        info!(
            name = %self.config.container_name,
            ip = %self.config.overlay_ip,
            "sidecar wireguard interface configured"
        );
        Ok(())
    }

    pub async fn down(&self) -> Result<()> {
        let stop_opts = StopContainerOptionsBuilder::default().t(5).build();

        match self
            .docker
            .stop_container(&self.config.container_name, Some(stop_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => {}
            Err(e) => return Err(Error::operation("sidecar stop", e.to_string())),
        }

        let remove_opts = RemoveContainerOptionsBuilder::default().build();
        match self
            .docker
            .remove_container(&self.config.container_name, Some(remove_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(Error::operation("sidecar remove", e.to_string())),
        }

        info!(name = %self.config.container_name, "sidecar container stopped");
        Ok(())
    }

    async fn remove_existing(&self) {
        docker_force_remove(&self.docker, &self.config.container_name).await;
    }

    async fn exec(&self, cmd: &[&str]) -> Result<()> {
        docker_exec_capture(
            &self.docker,
            &self.config.container_name,
            cmd,
            "sidecar exec",
        )
        .await
        .map(|_| ())
    }
}

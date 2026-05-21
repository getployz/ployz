use bollard::Docker;
use std::net::SocketAddr;

use crate::error::{Error, Result};

use super::{DockerWireGuard, DockerWireGuardBuilder};
use crate::mesh::wireguard::bridge::OutboundForward;
use crate::mesh::wireguard::config::WgPaths;

impl DockerWireGuardBuilder {
    #[must_use]
    pub fn image(mut self, image: &str) -> Self {
        self.image = image.to_string();
        self
    }

    #[must_use]
    pub fn listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    #[must_use]
    pub fn with_bridge_forward(mut self, local_addr: SocketAddr, overlay_dest: SocketAddr) -> Self {
        self.outbound_forwards.push(OutboundForward {
            local_addr,
            overlay_dest,
        });
        self
    }

    #[must_use]
    pub fn expose_tcp(mut self, port: u16) -> Self {
        self.exposed_tcp_ports.push(port);
        self
    }

    pub async fn build(self) -> Result<DockerWireGuard> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        docker
            .ping()
            .await
            .map_err(|e| Error::operation("docker ping", e.to_string()))?;

        let paths = WgPaths::new(&self.data_dir);

        let public_key_bytes =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(self.private_key.0))
                .to_bytes();

        Ok(DockerWireGuard {
            docker,
            container_name: self.container_name,
            image: self.image,
            paths,
            private_key: self.private_key,
            public_key_bytes,
            overlay_ip: self.overlay_ip,
            listen_port: self.listen_port,
            outbound_forwards: self.outbound_forwards,
            exposed_tcp_ports: self.exposed_tcp_ports,
            bridge: tokio::sync::Mutex::new(None),
            bridge_overlay_ip: tokio::sync::Mutex::new(None),
            extra_peers: tokio::sync::Mutex::new(Vec::new()),
        })
    }
}

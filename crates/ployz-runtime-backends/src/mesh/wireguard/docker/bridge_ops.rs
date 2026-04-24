use std::net::Ipv4Addr;

use tracing::info;

use crate::error::{Error, Result};
use crate::mesh::wireguard::bridge::OverlayBridge;
use crate::mesh::wireguard::config::{BridgePeerInfo, encode_key};

use super::{DockerWireGuard, INTERFACE_NAME, PERSISTENT_KEEPALIVE_SECS};

impl DockerWireGuard {
    pub async fn add_sidecar_peer(&self, pubkey: [u8; 32], overlay_ip: Ipv4Addr) -> Result<()> {
        let pubkey_b64 = encode_key(&pubkey);
        let allowed = format!("{overlay_ip}/32");
        let keepalive_secs = PERSISTENT_KEEPALIVE_SECS.to_string();

        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &pubkey_b64,
            "allowed-ips",
            &allowed,
            "persistent-keepalive",
            &keepalive_secs,
        ])
        .await?;

        self.extra_peers.lock().await.push(BridgePeerInfo {
            public_key: pubkey,
            allowed_ips: vec![allowed],
        });

        info!(%overlay_ip, "added sidecar peer to backbone");
        Ok(())
    }

    pub async fn remove_sidecar_peer(&self, pubkey: &[u8; 32]) -> Result<()> {
        let pubkey_b64 = encode_key(pubkey);

        self.exec_in_container(&["wg", "set", INTERFACE_NAME, "peer", &pubkey_b64, "remove"])
            .await?;

        self.extra_peers
            .lock()
            .await
            .retain(|p| &p.public_key != pubkey);

        info!("removed sidecar peer from backbone");
        Ok(())
    }

    pub(super) async fn start_bridge(&self) -> Result<()> {
        if self.outbound_forwards.is_empty() {
            return Ok(());
        }

        if self.bridge.lock().await.is_some() {
            info!(name = %self.container_name, "bridge already running");
            return Ok(());
        }

        let container_pubkey_bytes = self.public_key_bytes;

        let (bridge_secret, bridge_pub_bytes, bridge_overlay_ip) =
            OverlayBridge::generate_keypair();

        let bridge_pubkey_b64 = encode_key(&bridge_pub_bytes);
        let bridge_allowed = format!("{}/128", bridge_overlay_ip.0);
        let keepalive_secs = PERSISTENT_KEEPALIVE_SECS.to_string();

        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &bridge_pubkey_b64,
            "allowed-ips",
            &bridge_allowed,
            "persistent-keepalive",
            &keepalive_secs,
        ])
        .await?;

        info!(bridge_ip = %bridge_overlay_ip, "registered bridge as WG peer");

        let peer_endpoint = self.bridge_peer_endpoint();

        let bridge = OverlayBridge::start(
            bridge_secret,
            &container_pubkey_bytes,
            self.overlay_ip,
            peer_endpoint,
            self.outbound_forwards.clone(),
        )
        .await
        .map_err(|e| Error::operation("bridge start", e.to_string()))?;

        let peer_info = BridgePeerInfo {
            public_key: bridge_pub_bytes,
            allowed_ips: vec![bridge_allowed],
        };

        self.extra_peers.lock().await.push(peer_info);
        *self.bridge_overlay_ip.lock().await = Some(bridge_overlay_ip);
        *self.bridge.lock().await = Some(bridge);

        Ok(())
    }
}

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::model::{MachineRecord, PrivateKey};

use super::PERSISTENT_KEEPALIVE_SECS;

/// Filesystem paths for a WireGuard data directory.
#[derive(Debug, Clone)]
pub struct WgPaths {
    pub dir: PathBuf,
    pub sync_config: PathBuf,
    pub private_key_file: PathBuf,
}

impl WgPaths {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("wireguard");
        Self {
            sync_config: dir.join("wg0-sync.conf"),
            private_key_file: dir.join("private.key"),
            dir,
        }
    }

    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)
    }
}

/// Write the base64-encoded private key to a file (for `wg set ... private-key`).
pub fn write_private_key(paths: &WgPaths, private_key: &PrivateKey) -> io::Result<()> {
    paths.ensure_dir()?;
    fs::write(&paths.private_key_file, encode_key(&private_key.0))
}

/// Extra peer entry for the bridge (not a mesh peer).
pub struct BridgePeerInfo {
    pub public_key: [u8; 32],
    pub allowed_ips: Vec<String>,
}

fn render_peer(buf: &mut String, peer: &MachineRecord) {
    let _ = writeln!(buf, "[Peer]");
    let _ = writeln!(buf, "PublicKey = {}", encode_key(&peer.public_key.0));
    let _ = writeln!(buf, "AllowedIPs = {}", peer.allowed_cidrs().join(", "));

    if let Some(endpoint) = peer.endpoints.first() {
        let _ = writeln!(buf, "Endpoint = {endpoint}");
    }
    let _ = writeln!(buf, "PersistentKeepalive = {PERSISTENT_KEEPALIVE_SECS}");
}

fn render_bridge_peer(buf: &mut String, bridge: &BridgePeerInfo) {
    let _ = writeln!(buf, "[Peer]");
    let _ = writeln!(buf, "PublicKey = {}", encode_key(&bridge.public_key));
    let _ = writeln!(buf, "AllowedIPs = {}", bridge.allowed_ips.join(", "));
    let _ = writeln!(buf, "PersistentKeepalive = {PERSISTENT_KEEPALIVE_SECS}");
}

/// Write a sync config that includes extra peers (bridge + sidecars), protected from syncconf removal.
pub fn write_sync_config_with_extra_peers(
    paths: &WgPaths,
    private_key: &PrivateKey,
    listen_port: u16,
    peers: &[MachineRecord],
    extra_peers: &[&BridgePeerInfo],
) -> io::Result<()> {
    paths.ensure_dir()?;
    let mut buf = String::with_capacity(512);
    let _ = writeln!(buf, "[Interface]");
    let _ = writeln!(buf, "PrivateKey = {}", encode_key(&private_key.0));
    let _ = writeln!(buf, "ListenPort = {listen_port}");

    for extra in extra_peers {
        let _ = writeln!(buf);
        render_bridge_peer(&mut buf, extra);
    }

    for peer in peers {
        let _ = writeln!(buf);
        render_peer(&mut buf, peer);
    }

    fs::write(&paths.sync_config, buf)
}

#[must_use]
pub fn encode_key(key: &[u8; 32]) -> String {
    BASE64.encode(key)
}

pub fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    let bytes = BASE64
        .decode(b64)
        .map_err(|e| format!("invalid base64 key: {e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("key must be 32 bytes, got {}", v.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_key_encoding() {
        let key = [0xab; 32];
        let encoded = encode_key(&key);
        let decoded = decode_key(&encoded).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn sync_config_with_extra_peers() {
        let privkey = PrivateKey([1; 32]);
        let paths = WgPaths::new(std::path::Path::new("/tmp/ployz-test-extra-peers"));

        let sidecar1 = BridgePeerInfo {
            public_key: [0xaa; 32],
            allowed_ips: vec!["10.210.0.3/32".to_string()],
        };
        let sidecar2 = BridgePeerInfo {
            public_key: [0xbb; 32],
            allowed_ips: vec!["10.210.0.4/32".to_string()],
        };

        let _ = std::fs::create_dir_all(&paths.dir);
        write_sync_config_with_extra_peers(&paths, &privkey, 51820, &[], &[&sidecar1, &sidecar2])
            .unwrap();

        let content = std::fs::read_to_string(&paths.sync_config).unwrap();
        assert!(content.contains(&encode_key(&sidecar1.public_key)));
        assert!(content.contains(&encode_key(&sidecar2.public_key)));
        assert!(content.contains("10.210.0.3/32"));
        assert!(content.contains("10.210.0.4/32"));
        let peer_count = content.matches("[Peer]").count();
        assert_eq!(peer_count, 2);

        let _ = std::fs::remove_dir_all(&paths.dir);
    }
}

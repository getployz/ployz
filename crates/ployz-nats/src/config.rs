use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};

use ployz_types::model::MachineRole;

pub const CLIENT_PORT: u16 = 4222;
pub const ROUTE_PORT: u16 = 6222;
pub const LEAFNODE_PORT: u16 = 7422;
pub const MONITOR_PORT: u16 = 8222;
pub const HUB_DOMAIN: &str = "hub";

#[must_use]
pub fn leaf_domain(overlay_ip: Ipv6Addr) -> String {
    format!("leaf-{}", overlay_ip.to_string().replace(':', "-"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
}

pub fn write_config(paths: &Paths, config: &ServerConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(&paths.root)?;
    std::fs::create_dir_all(&paths.data)?;
    std::fs::write(&paths.config, config.render())
}

impl Paths {
    #[must_use]
    pub fn new(network_dir: &Path) -> Self {
        let root = network_dir.join("nats");
        Self {
            config: root.join("nats.conf"),
            data: root.join("data"),
            root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRoute {
    pub overlay_ip: Ipv6Addr,
}

impl PeerRoute {
    #[must_use]
    pub fn route_url(&self) -> String {
        format!("nats://[{}]:{}", self.overlay_ip, ROUTE_PORT)
    }

    #[must_use]
    pub fn leafnode_url(&self) -> String {
        format!("nats://[{}]:{}", self.overlay_ip, LEAFNODE_PORT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub server_name: String,
    pub cluster_name: String,
    pub role: MachineRole,
    pub overlay_ip: Ipv6Addr,
    pub storage_peers: Vec<PeerRoute>,
    pub data_dir: PathBuf,
}

impl ServerConfig {
    #[must_use]
    pub fn render(&self) -> String {
        let mut config = format!(
            "server_name: {}\nlisten: \"[{}]:{}\"\nhttp: \"127.0.0.1:{}\"\n",
            self.server_name, self.overlay_ip, CLIENT_PORT, MONITOR_PORT
        );
        match self.role {
            MachineRole::StorageCandidate => {
                config.push_str(&format!(
                    "jetstream {{\n  domain: {}\n  store_dir: \"{}\"\n}}\n",
                    HUB_DOMAIN,
                    self.data_dir.display()
                ));
                if !self.storage_peers.is_empty() {
                    config.push_str(&format!(
                        "cluster {{\n  name: {}\n  listen: \"[{}]:{}\"\n",
                        self.cluster_name, self.overlay_ip, ROUTE_PORT
                    ));
                    config.push_str("  routes: [\n");
                    for peer in &self.storage_peers {
                        config.push_str(&format!("    \"{}\"\n", peer.route_url()));
                    }
                    config.push_str("  ]\n");
                    config.push_str("}\n");
                }
                config.push_str(&format!(
                    "leafnodes {{\n  listen: \"[{}]:{}\"\n}}\n",
                    self.overlay_ip, LEAFNODE_PORT
                ));
            }
            MachineRole::Mirror => {
                config.push_str(&format!(
                    "jetstream {{\n  domain: {}\n  store_dir: \"{}\"\n}}\n",
                    self.leaf_domain(),
                    self.data_dir.display()
                ));
                config.push_str("leafnodes {\n  remotes: [\n");
                for peer in &self.storage_peers {
                    config.push_str(&format!("    {{ url: \"{}\" }}\n", peer.leafnode_url()));
                }
                config.push_str("  ]\n}\n");
            }
            MachineRole::Leaf => {
                config.push_str("leafnodes {\n  remotes: [\n");
                for peer in &self.storage_peers {
                    config.push_str(&format!("    {{ url: \"{}\" }}\n", peer.leafnode_url()));
                }
                config.push_str("  ]\n}\n");
            }
        }
        config
    }

    #[must_use]
    pub fn leaf_domain(&self) -> String {
        leaf_domain(self.overlay_ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitoring_binds_to_loopback_only() {
        let rendered = ServerConfig {
            server_name: "m1".into(),
            cluster_name: "ployz-alpha".into(),
            role: MachineRole::StorageCandidate,
            overlay_ip: "fd00::1".parse().expect("valid ip"),
            storage_peers: Vec::new(),
            data_dir: PathBuf::from("/data"),
        }
        .render();
        assert!(rendered.contains("http: \"127.0.0.1:8222\""));
        assert!(!rendered.contains("http: \"[fd00::1]:8222\""));
    }

    #[test]
    fn single_storage_node_does_not_enable_jetstream_cluster_mode() {
        let rendered = ServerConfig {
            server_name: "m1".into(),
            cluster_name: "ployz-alpha".into(),
            role: MachineRole::StorageCandidate,
            overlay_ip: "fd00::1".parse().expect("valid ip"),
            storage_peers: Vec::new(),
            data_dir: PathBuf::from("/data"),
        }
        .render();
        assert!(!rendered.contains("cluster {"));
        assert!(rendered.contains("jetstream {"));
        assert!(rendered.contains("domain: hub"));
        assert!(rendered.contains("leafnodes {"));
    }

    #[test]
    fn leaf_node_does_not_enable_local_jetstream() {
        let rendered = ServerConfig {
            server_name: "leaf-1".into(),
            cluster_name: "ployz-alpha".into(),
            role: MachineRole::Leaf,
            overlay_ip: "fd00::10".parse().expect("valid ip"),
            storage_peers: vec![PeerRoute {
                overlay_ip: "fd00::1".parse().expect("valid ip"),
            }],
            data_dir: PathBuf::from("/data"),
        }
        .render();
        assert!(!rendered.contains("jetstream {"));
        assert!(rendered.contains("url: \"nats://[fd00::1]:7422\""));
    }

    #[test]
    fn mirror_node_enables_local_jetstream_and_leafnode_remotes() {
        let rendered = ServerConfig {
            server_name: "mirror-1".into(),
            cluster_name: "ployz-alpha".into(),
            role: MachineRole::Mirror,
            overlay_ip: "fd00::10".parse().expect("valid ip"),
            storage_peers: vec![PeerRoute {
                overlay_ip: "fd00::1".parse().expect("valid ip"),
            }],
            data_dir: PathBuf::from("/data"),
        }
        .render();
        assert!(rendered.contains("jetstream {"));
        assert!(rendered.contains("domain: leaf-fd00--10"));
        assert!(rendered.contains("url: \"nats://[fd00::1]:7422\""));
        assert!(!rendered.contains("cluster {"));
    }

    #[test]
    fn leaf_nodes_use_leafnode_remotes_not_cluster_routes() {
        let rendered = ServerConfig {
            server_name: "leaf-1".into(),
            cluster_name: "ployz-alpha".into(),
            role: MachineRole::Leaf,
            overlay_ip: "fd00::10".parse().expect("valid ip"),
            storage_peers: vec![PeerRoute {
                overlay_ip: "fd00::1".parse().expect("valid ip"),
            }],
            data_dir: PathBuf::from("/data"),
        }
        .render();
        assert!(rendered.contains("url: \"nats://[fd00::1]:7422\""));
        assert!(!rendered.contains("cluster {"));
    }
}

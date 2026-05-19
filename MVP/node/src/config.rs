use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPeerConfig {
    pub node_id: Option<String>,
    pub principal_id: String,
    pub author_key_hex: String,
    pub p2panda_ticket: String,
    pub invite_id: Option<String>,
}

impl BootstrapPeerConfig {
    #[must_use]
    pub fn new(
        node_id: Option<String>,
        principal_id: impl Into<String>,
        author_key_hex: impl Into<String>,
        p2panda_ticket: impl Into<String>,
        invite_id: Option<String>,
    ) -> Self {
        Self {
            node_id,
            principal_id: principal_id.into(),
            author_key_hex: author_key_hex.into(),
            p2panda_ticket: p2panda_ticket.into(),
            invite_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePaths {
    pub state_dir: PathBuf,
    pub state_file: PathBuf,
    pub fact_store: PathBuf,
    pub projection_db: PathBuf,
    pub gateway_snapshot: PathBuf,
    pub dns_snapshot: PathBuf,
}

impl NodePaths {
    #[must_use]
    pub fn for_state_dir(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        Self {
            state_file: state_dir.join("node-state.json"),
            fact_store: state_dir.join("facts.sqlite"),
            projection_db: state_dir.join("projections.sqlite"),
            gateway_snapshot: state_dir.join("gateway.snapshot"),
            dns_snapshot: state_dir.join("dns.snapshot"),
            state_dir,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOptions {
    pub state_dir: PathBuf,
    pub island: String,
    pub node_id: Option<String>,
}

impl InitOptions {
    #[must_use]
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            island: "default".to_string(),
            node_id: None,
        }
    }

    #[must_use]
    pub fn with_island(mut self, island: impl Into<String>) -> Self {
        self.island = island.into();
        self
    }

    #[must_use]
    pub fn with_node_id(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedInitOptions {
    pub state_dir: PathBuf,
    pub island: String,
    pub node_id: String,
    pub invite_id: String,
    pub invite_secret: String,
    pub invite_expires_at_ms: u64,
    pub p2panda_network_id_hex: String,
    pub p2panda_topic_hex: String,
    pub bootstrap_peer: BootstrapPeerConfig,
}

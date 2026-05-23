use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{NodeError, NodeResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub island_id: String,
    #[serde(default)]
    pub bootstrap_node_id: Option<String>,
    pub p2panda_network_id_hex: String,
    pub p2panda_topic_hex: String,
    pub bootstrap_ticket: String,
    pub bootstrap_principal_id: String,
    pub bootstrap_author_key_hex: String,
    pub invite_id: String,
    pub invite_secret: String,
    pub expires_at_ms: u64,
}

impl InviteToken {
    pub fn encode(&self) -> NodeResult<String> {
        serde_json::to_string(self).map_err(|source| NodeError::EncodeInviteToken { source })
    }

    pub fn decode(value: &str) -> NodeResult<Self> {
        serde_json::from_str(value).map_err(|source| NodeError::DecodeInviteToken { source })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRequest {
    pub island_id: String,
    pub node_id: String,
    pub principal_id: String,
    pub p2panda_ticket: String,
    pub author_key_hex: String,
    pub wg_public_key: String,
    pub wg_overlay_ip: String,
    pub invite_id: String,
    pub invite_secret: String,
    pub invite_expires_at_ms: u64,
}

impl AdmissionRequest {
    pub fn encode(&self) -> NodeResult<String> {
        serde_json::to_string(self).map_err(|source| NodeError::EncodeAdmissionRequest { source })
    }

    pub fn decode(value: &str) -> NodeResult<Self> {
        serde_json::from_str(value).map_err(|source| NodeError::DecodeAdmissionRequest { source })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReport {
    pub node_id: String,
    pub principal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOptions {
    pub run_for: Duration,
    pub import_idle: Duration,
    pub control_socket: Option<PathBuf>,
    pub wireguard: DaemonWireGuardMode,
    pub runtime: DaemonRuntimeMode,
}

impl DaemonOptions {
    #[must_use]
    pub fn new(run_for: Duration) -> Self {
        Self {
            run_for,
            import_idle: Duration::from_millis(50),
            control_socket: None,
            wireguard: DaemonWireGuardMode::Memory,
            runtime: DaemonRuntimeMode::Process,
        }
    }

    #[must_use]
    pub fn with_control_socket(mut self, control_socket: impl Into<PathBuf>) -> Self {
        self.control_socket = Some(control_socket.into());
        self
    }

    #[must_use]
    pub fn with_linux_wireguard(mut self, ifname: impl Into<String>) -> Self {
        self = self.with_linux_wireguard_listen_port(ifname, 51820);
        self
    }

    #[must_use]
    pub fn with_linux_wireguard_listen_port(
        mut self,
        ifname: impl Into<String>,
        listen_port: u16,
    ) -> Self {
        self.wireguard = DaemonWireGuardMode::Linux {
            ifname: ifname.into(),
            listen_port,
        };
        self
    }

    #[must_use]
    pub fn with_docker_runtime(
        mut self,
        image: impl Into<String>,
        service_port: u16,
        command: Option<Vec<String>>,
    ) -> Self {
        self.runtime = DaemonRuntimeMode::Docker {
            image: image.into(),
            service_port,
            command,
        };
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonWireGuardMode {
    Memory,
    Linux { ifname: String, listen_port: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonRuntimeMode {
    Process,
    Docker {
        image: String,
        service_port: u16,
        command: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonReport {
    pub node_id: String,
    pub ticket: String,
    pub imported_batches: u64,
    pub imported_operations: u64,
    pub node_agent_handlers: usize,
    pub wireguard_backend: String,
    pub wireguard_applied_revision: Option<u64>,
}

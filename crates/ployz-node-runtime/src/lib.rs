mod node_clients;

use std::future::Future;
use std::time::Duration;

pub use node_clients::{
    NodeClientError, NodeClientRegistry, NodeCommand, NodePeerClient, NodeRpcError,
    NodeRpcErrorKind, NodeRpcPolicy, VolumeZfsNodeClient, VolumeZfsRpcOperation,
    VolumeZfsRpcTransport,
};
use ployz_supervision::{HealthRegistry, Supervisor};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub node_id: String,
    pub shutdown_deadline: Duration,
}

pub struct NodeRuntime {
    config: RuntimeConfig,
    supervisor: Supervisor,
}

impl NodeRuntime {
    #[must_use]
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            supervisor: Supervisor::new(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    #[must_use]
    pub fn health(&self) -> HealthRegistry {
        self.supervisor.health()
    }

    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.supervisor.shutdown_token()
    }

    pub fn spawn_component<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.supervisor.spawn(name, future);
    }

    pub async fn shutdown(self) -> Result<(), String> {
        self.supervisor
            .shutdown(self.config.shutdown_deadline)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_owns_component_health_and_shutdown() {
        let runtime = NodeRuntime::new(RuntimeConfig {
            node_id: "node-a".to_string(),
            shutdown_deadline: Duration::from_secs(1),
        });
        let health = runtime.health();

        runtime.spawn_component("component", async { Ok(()) });
        runtime.shutdown().await.expect("shutdown");

        assert!(health.get("component").is_some());
    }
}

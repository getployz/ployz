//! Secured NATS fixture for e2e tests: wraps the shared connected
//! [`ployz_test_support::nats::TestNats`] with the control-process config
//! the e2e scenarios launch ployzd with.

use ployz_core::ids::NodeId;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::nats_process::NatsServerRuntime;

pub struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    work_dir: tempfile::TempDir,
}

impl TestNats {
    /// Starts a secured server with one minted Node user per supplied id and
    /// connects the Controller and User clients.
    pub async fn start_with_nodes(node_ids: &[NodeId]) -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_nodes(node_ids).await;
        let work_dir = tempfile::TempDir::new().expect("test work dir creates");

        Self {
            connected,
            work_dir,
        }
    }

    /// The control-plane client (Controller principal).
    #[must_use]
    pub fn controller_client(&self) -> async_nats::Client {
        self.connected.controller.clone()
    }

    /// The operator client (User principal) the API client requests with.
    #[must_use]
    pub fn user_client(&self) -> async_nats::Client {
        self.connected.user.clone()
    }

    /// A client authenticated as the machine's Node user. Node runtimes,
    /// gateway processes, and observation writers connect with this.
    pub async fn node_client(&self, node_id: &NodeId) -> async_nats::Client {
        self.connected.node_client(node_id).await
    }

    /// A control process config pointed at this fixture's server and
    /// authorized-users file.
    #[must_use]
    pub fn control_config(&self, core_node_id: NodeId) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerRuntime::External(self.connected.server.client_url().clone()),
            core_node_id,
            self.connected.server.controller_config(),
        )
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.connected.server.authorized_users_path().to_path_buf(),
            node_seed_file: self.work_dir.path().join("node.seed"),
        })
    }
}

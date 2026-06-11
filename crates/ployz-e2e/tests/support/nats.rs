//! Secured NATS fixture for e2e tests: wraps the shared
//! [`ployz_test_support::nats::SecuredTestNats`] server with the
//! per-principal clients the e2e scenarios connect as.

use std::error::Error;
use std::time::Duration;

use ployz_core::ids::NodeId;
use ployz_nats::connect::connect_authenticated;
use ployz_test_support::nats::SecuredTestNats;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::nats_process::NatsServerRuntime;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type FixtureError = Box<dyn Error + Send + Sync>;

pub struct TestNats {
    nats: SecuredTestNats,
    controller: async_nats::Client,
    user: async_nats::Client,
    work_dir: tempfile::TempDir,
}

impl TestNats {
    /// Starts a secured server with one minted Node user per supplied id and
    /// connects the Controller and User clients.
    pub async fn start_with_nodes(node_ids: &[NodeId]) -> Result<Self, FixtureError> {
        let nats = SecuredTestNats::start_with_nodes(node_ids).await?;
        let controller = connect_authenticated(&nats.controller_config(), CONNECT_TIMEOUT).await?;
        let user = connect_authenticated(&nats.user_config(), CONNECT_TIMEOUT).await?;
        let work_dir = tempfile::TempDir::new()?;

        Ok(Self {
            nats,
            controller,
            user,
            work_dir,
        })
    }

    /// The control-plane client (Controller principal).
    #[must_use]
    pub fn controller_client(&self) -> async_nats::Client {
        self.controller.clone()
    }

    /// The operator client (User principal) the API client requests with.
    #[must_use]
    pub fn user_client(&self) -> async_nats::Client {
        self.user.clone()
    }

    /// A client authenticated as the machine's Node user. Node runtimes,
    /// gateway processes, and observation writers connect with this.
    pub async fn node_client(&self, node_id: &NodeId) -> Result<async_nats::Client, FixtureError> {
        let Some(config) = self.nats.node_config(node_id) else {
            return Err(format!("fixture has no minted node user for {}", node_id.as_str()).into());
        };
        connect_authenticated(&config, CONNECT_TIMEOUT)
            .await
            .map_err(Into::into)
    }

    /// A control process config pointed at this fixture's server and
    /// authorized-users file.
    #[must_use]
    pub fn control_config(&self, core_node_id: NodeId) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerRuntime::External(self.nats.client_url().clone()),
            core_node_id,
            self.nats.controller_config(),
        )
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.nats.authorized_users_path().to_path_buf(),
            node_seed_file: self.work_dir.path().join("node.seed"),
        })
    }
}

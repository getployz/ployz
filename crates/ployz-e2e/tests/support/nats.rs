//! Secured NATS fixture for e2e tests: wraps the shared connected
//! [`ployz_test_support::nats::TestNats`] with the control-process config
//! the e2e scenarios launch ployzd with.

use ployz_core::ids::MachineId;
use ployzd::adapters::nats_authorization::SignalNatsReloadRunner;
use ployzd::adapters::nats_server::NatsServerLaunch;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::roles::control::{ControlProcessError, RunningControlProcess};

pub struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    work_dir: tempfile::TempDir,
}

impl TestNats {
    /// Starts a secured server with one minted Machine user per supplied id and
    /// connects the Controller and Operator clients.
    pub async fn start_with_machines(machine_ids: &[MachineId]) -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_machines(machine_ids).await;
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

    /// The operator client (Operator principal) the API client requests with.
    #[must_use]
    pub fn user_client(&self) -> async_nats::Client {
        self.connected.user.clone()
    }

    /// A client authenticated as the machine's Machine user. Machine runtimes,
    /// gateway processes, and observation writers connect with this.
    pub async fn machine_client(&self, machine_id: &MachineId) -> async_nats::Client {
        self.connected.machine_client(machine_id).await
    }

    /// A control process config pointed at this fixture's server and
    /// authorized-users file.
    #[must_use]
    pub fn control_config(&self, core_machine_id: MachineId) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerLaunch::External(self.connected.server.client_url().clone()),
            core_machine_id,
            self.connected.server.controller_config(),
        )
        .with_core_db_path(self.work_dir.path().join("ployz-core.db"))
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.connected.server.authorized_users_path().to_path_buf(),
            machine_seed_file: self.work_dir.path().join("machine.seed"),
        })
    }

    pub async fn start_control(
        &self,
        config: &ControlProcessConfig,
    ) -> Result<RunningControlProcess, ControlProcessError> {
        ployzd::roles::control::start_control_process_with_client_and_reload(
            self.controller_client(),
            config,
            SignalNatsReloadRunner::new(self.connected.server.server_pid()),
        )
        .await
    }
}

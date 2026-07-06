//! Control-process wiring over the shared connected NATS fixture: the
//! control runtime pointed at the fixture server, and the reload-runner
//! test double used to drive machine-add minting.

use ployz_core::ids::MachineId;
use ployz_core::install::{MachineBootstrapUrl, MachineJoinBundle, MachineJoinTemplate};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinToken,
};
use ployz_test_support::fixtures::machine_join_material;
use ployz_test_support::ids::machine_id;
use ployz_test_support::nats::SecuredTestNats;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::adapters::nats_authorization::{
    NatsReloadEvidence, NatsReloadOutcome, NatsReloadRunner, SignalNatsReloadRunner,
};
use ployzd::adapters::nats_server::NatsServerRuntime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct TestNats {
    pub connected: ployz_test_support::nats::TestNats,
    pub work_dir: tempfile::TempDir,
}

impl TestNats {
    pub async fn start() -> Self {
        Self::start_with_machines(&[]).await
    }

    pub async fn start_with_machines(machine_ids: &[MachineId]) -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_machines(machine_ids).await;
        let work_dir = tempfile::TempDir::new().expect("test work dir creates");

        Self {
            connected,
            work_dir,
        }
    }

    pub fn server(&self) -> &SecuredTestNats {
        &self.connected.server
    }

    pub fn api(&self) -> OperationApiClient {
        self.connected.api()
    }

    pub async fn machine_client(&self, machine_id: &MachineId) -> async_nats::Client {
        self.connected.machine_client(machine_id).await
    }

    pub fn reload_runner(&self) -> RecordingReload {
        RecordingReload::signal(self.server().server_pid())
    }

    pub fn control_config(&self) -> ControlProcessConfig {
        self.control_config_without_join_template()
            .with_machine_bootstrap(
                MachineAddBootstrapConfig::new(
                    MachineBootstrapUrl::try_new(
                        ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL,
                    )
                    .expect("valid bootstrap url"),
                )
                .with_join_material(
                    machine_join_template(self),
                    ployz_core::install::MachineJoinSecretDelivery {
                        nats_credentials: self.server().join_seed().clone(),
                    },
                ),
            )
    }

    pub fn control_config_without_join_template(&self) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerRuntime::External(self.server().client_url().clone()),
            machine_id("core_1"),
            self.server().controller_config(),
        )
        .with_core_db_path(self.work_dir.path().join("ployz-core.db"))
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.server().authorized_users_path().to_path_buf(),
            machine_seed_file: self.work_dir.path().join("machine.seed"),
        })
    }

    pub async fn start_control(
        &self,
        config: &ControlProcessConfig,
    ) -> ployzd::control_runtime::RunningControlRuntime {
        self.start_control_with_reload(config, self.reload_runner())
            .await
    }

    pub async fn start_control_with_reload(
        &self,
        config: &ControlProcessConfig,
        reload: RecordingReload,
    ) -> ployzd::control_runtime::RunningControlRuntime {
        ployzd::control_runtime::start_control_runtime_with_client_and_reload(
            self.connected.controller.clone(),
            config,
            reload,
        )
        .await
        .expect("control runtime starts")
    }
}

/// Records reload outcomes; signals the fixture server, fails on purpose,
/// or blocks behind a release gate to prove handler/reload ordering.
#[derive(Clone)]
pub struct RecordingReload {
    behavior: ReloadBehavior,
    outcomes: Arc<Mutex<Vec<NatsReloadOutcome>>>,
}

#[derive(Clone)]
enum ReloadBehavior {
    Signal(u32),
    GatedSignal { pid: u32, released: Arc<AtomicBool> },
    Fail,
}

impl RecordingReload {
    pub fn signal(pid: u32) -> Self {
        Self {
            behavior: ReloadBehavior::Signal(pid),
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn gated_signal(pid: u32) -> Self {
        Self {
            behavior: ReloadBehavior::GatedSignal {
                pid,
                released: Arc::new(AtomicBool::new(false)),
            },
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn failing() -> Self {
        Self {
            behavior: ReloadBehavior::Fail,
            outcomes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn release(&self) {
        if let ReloadBehavior::GatedSignal { released, .. } = &self.behavior {
            released.store(true, Ordering::SeqCst);
        }
    }

    pub fn outcomes(&self) -> Vec<NatsReloadOutcome> {
        self.outcomes
            .lock()
            .expect("reload outcome lock is not poisoned")
            .clone()
    }
}

impl NatsReloadRunner for RecordingReload {
    fn reload(&self) -> NatsReloadOutcome {
        let outcome = match &self.behavior {
            ReloadBehavior::Signal(pid) => SignalNatsReloadRunner::new(*pid).reload(),
            ReloadBehavior::GatedSignal { pid, released } => {
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                SignalNatsReloadRunner::new(*pid).reload()
            }
            ReloadBehavior::Fail => NatsReloadOutcome::Failed(NatsReloadEvidence {
                command: "test-reload".to_owned(),
                output: "reload refused by test".to_owned(),
            }),
        };
        self.outcomes
            .lock()
            .expect("reload outcome lock is not poisoned")
            .push(outcome.clone());
        outcome
    }
}

/// Keeper-style bounded redeem retry: not-ready is retried, anything else
/// is a test failure.
pub async fn redeem_when_ready(
    api: &OperationApiClient,
    join_token: &MachineJoinToken,
) -> MachineJoinRedeemed {
    for _ in 0..200 {
        match api
            .machine_join_redeem(&MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            })
            .await
        {
            Ok(redeemed) => return redeemed,
            Err(OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { .. },
                ..
            }) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => panic!("redeem failed: {error:?}"),
        }
    }
    panic!("machine-add material did not become ready");
}

/// The canonical join material rendered against this fixture's live server
/// URL and CA.
pub fn machine_join_template(nats: &TestNats) -> MachineJoinTemplate {
    let ca_pem = std::fs::read_to_string(nats.server().ca_path()).expect("fixture CA is readable");
    MachineJoinTemplate {
        join_bundle: MachineJoinBundle {
            material: machine_join_material(nats.server().client_url().as_str(), &ca_pem),
        },
    }
}

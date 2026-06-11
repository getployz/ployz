//! Shared control-runtime fixture: a secured (TLS + NKey-authorized)
//! `nats-server` plus the control process wired against it, and the
//! reload-runner test double used to drive machine-add minting.

use async_nats::jetstream;
use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineBootstrapUrl, MachineJoinArtifact, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl, MachineJoinTemplate,
    MachineJoinTrustedNats,
};
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_nats::connect::connect_authenticated;
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinToken,
};
use ployz_test_support::nats::SecuredTestNats;
use ployzd::config::{ControlNatsAuthorizationConfig, ControlProcessConfig};
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::nats_authorization::{
    NatsReloadEvidence, NatsReloadOutcome, NatsReloadRunner, SignalNatsReloadRunner,
};
use ployzd::nats_process::NatsServerRuntime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const FIXTURE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct TestNats {
    pub nats: SecuredTestNats,
    pub client: async_nats::Client,
    pub user_client: async_nats::Client,
    pub jetstream: jetstream::Context,
    pub work_dir: tempfile::TempDir,
}

impl TestNats {
    pub async fn start() -> Self {
        Self::start_with_nodes(&[]).await
    }

    pub async fn start_with_nodes(node_ids: &[NodeId]) -> Self {
        let nats = SecuredTestNats::start_with_nodes(node_ids)
            .await
            .expect("secured test nats starts");
        let client = connect_authenticated(&nats.controller_config(), FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("controller connects");
        let user_client = connect_authenticated(&nats.user_config(), FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("operator connects");
        let jetstream = jetstream::new(client.clone());
        let work_dir = tempfile::TempDir::new().expect("test work dir creates");

        Self {
            nats,
            client,
            user_client,
            jetstream,
            work_dir,
        }
    }

    pub fn api(&self) -> OperationApiClient {
        OperationApiClient::new(self.user_client.clone())
    }

    pub async fn node_client(&self, node_id: &NodeId) -> async_nats::Client {
        let config = self
            .nats
            .node_config(node_id)
            .expect("fixture knows the node user");
        connect_authenticated(&config, FIXTURE_CONNECT_TIMEOUT)
            .await
            .expect("node connects")
    }

    pub fn reload_runner(&self) -> RecordingReload {
        RecordingReload::signal(self.nats.server_pid())
    }

    pub fn control_config(&self) -> ControlProcessConfig {
        self.control_config_without_join_template()
            .with_machine_bootstrap(
                MachineAddBootstrapConfig::new(
                    MachineBootstrapUrl::try_new("https://get.ployz.dev/ployz.sh")
                        .expect("valid bootstrap url"),
                )
                .with_join_template(machine_join_template(self)),
            )
    }

    pub fn control_config_without_join_template(&self) -> ControlProcessConfig {
        ControlProcessConfig::new(
            NatsServerRuntime::External(self.nats.client_url().clone()),
            node_id("core_1"),
            self.nats.controller_config(),
        )
        .with_nats_authorization(ControlNatsAuthorizationConfig {
            authorized_users_file: self.nats.authorized_users_path().to_path_buf(),
            node_seed_file: self.work_dir.path().join("node.seed"),
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
            self.client.clone(),
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

pub fn machine_join_template(nats: &TestNats) -> MachineJoinTemplate {
    let ca_pem = std::fs::read_to_string(nats.nats.ca_path()).expect("fixture CA is readable");
    MachineJoinTemplate {
        join_bundle: ployz_core::install::MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(
                    nats.nats.client_url().as_str(),
                )
                .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(ca_pem).expect("valid ca pem"),
                },
                ployzd: MachineJoinPloyzdArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
                        .expect("valid install path"),
                },
                ebpf_bytecode: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-tc")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new(
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                    )
                    .expect("valid install path"),
                },
                ebpf_ctl: MachineJoinArtifact {
                    version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                    source: InstallArtifactSource::try_new("/tmp/ployz-ebpf-ctl")
                        .expect("valid source"),
                    sha256: InstallSha256Digest::try_new(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid digest"),
                    install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployz-ebpf-ctl")
                        .expect("valid install path"),
                },
            },
        },
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
